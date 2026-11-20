using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Net.Http.Headers;
using System.Net.NetworkInformation;
using System.Runtime.InteropServices;
using System.Security;
using System.Text;
using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>One quota window; Severity is the provider's own level ("normal", "warning", ...) when it sends one.</summary>
public sealed record AiUsageWindow(string LabelKey, double Used, DateTimeOffset? ResetsAt, long? WindowSeconds, string? Severity = null, string? Group = null, string? GroupModels = null);
public sealed record AiUsageShare(string Name, double Used);
public sealed record AiUsageSpend(decimal Used, decimal? Limit, string Currency, double? Used01);

public sealed record AiUsageReport(AiProviders Provider, string? Plan, IReadOnlyList<AiUsageWindow> Windows, DateTimeOffset FetchedAt)
{
    public bool LimitReached { get; init; }
    /// <summary>Claude weekly usage split by surface, as a share of the weekly limit; only non-zero rows.</summary>
    public IReadOnlyList<AiUsageShare> Breakdown { get; init; } = [];
    public AiUsageSpend? Spend { get; init; }
    /// <summary>Codex credit balance; null when the account has no credits.</summary>
    public double? Credits { get; init; }
    public bool UnlimitedCredits { get; init; }
    public int ResetCredits { get; init; }
}

/// <summary>Borrowed CLI sign-in, read on each poll. Never refreshes, writes or caches credentials.</summary>
public static class AiUsageService
{
    private static readonly HttpClient Client = CreateClient();
    private const double MaxResetSeconds = 4e9;
    private const string ClaudeOAuthClientId = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    internal sealed record LoadResult(AiUsageReport? Report, string? Error, TimeSpan? RetryAfter = null);
    private sealed record Credential(string Token, string? Account = null, string? Plan = null, string? RefreshToken = null, string? FilePath = null);

    // Keep background quota queries non-interactive and out of the foreground.
    // CreateNoWindow applies to a console executable; Hidden also requests that
    // Windows not show its initial window if the target has a window subsystem.
    internal static ProcessStartInfo HiddenCliStartInfo(string executable, params string[] arguments)
    {
        var startInfo = new ProcessStartInfo(executable)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            WindowStyle = ProcessWindowStyle.Hidden,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8
        };
        foreach (var argument in arguments) startInfo.ArgumentList.Add(argument);
        return startInfo;
    }


    public static async Task<(AiUsageReport? Report, string? Error)> LoadAsync(AiProviders provider, CancellationToken cancellation)
    {
        var result = await LoadWithPolicyAsync(provider, cancellation);
        return (result.Report, result.Error);
    }

    internal static async Task<LoadResult> LoadWithPolicyAsync(AiProviders provider, CancellationToken cancellation)
    {
        if (provider is not (AiProviders.Claude or AiProviders.Codex or AiProviders.Antigravity or AiProviders.OpenCode or AiProviders.Copilot or AiProviders.DeepSeek or AiProviders.OpenRouter))
            return new(null, "ErrorAiUsageLoad");
        int? status = null;
        try
        {
            cancellation.ThrowIfCancellationRequested();
            if (!await Task.Run(NetworkInterface.GetIsNetworkAvailable, cancellation).ConfigureAwait(false))
                return new(null, "ErrorAiUsageLoad");
            var credential = await Task.Run(() => ReadCredential(provider), cancellation).ConfigureAwait(false);
            if (credential is null) return new(null, "ErrorAiUsageSignIn");
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            timeout.CancelAfter(TimeSpan.FromSeconds(20));
            if (provider == AiProviders.Claude && string.IsNullOrWhiteSpace(credential.Token))
            {
                credential = await RefreshClaudeCredentialAsync(credential, null, timeout.Token).ConfigureAwait(false);
                if (credential is null || string.IsNullOrWhiteSpace(credential.Token)) return new(null, "ErrorAiUsageSignIn");
            }
            if (provider == AiProviders.Antigravity && await TryLoadAntigravityCliAsync(credential.Plan, timeout.Token).ConfigureAwait(false) is { } cliReport)
                return new(cliReport, null);
            var response = await SendUsageRequestAsync(provider, credential, timeout.Token).ConfigureAwait(false);
            try
            {
                if (provider == AiProviders.Claude && response.StatusCode == HttpStatusCode.Unauthorized
                    && !string.IsNullOrWhiteSpace(credential.RefreshToken)
                    && await RefreshClaudeCredentialAsync(credential, null, timeout.Token).ConfigureAwait(false) is { Token.Length: > 0 } refreshed)
                {
                    credential = refreshed;
                    response.Dispose();
                    response = await SendUsageRequestAsync(provider, credential, timeout.Token).ConfigureAwait(false);
                }
                status = (int)response.StatusCode;
                if (!response.IsSuccessStatusCode)
                {
                    AppLog.Info($"AI usage ({provider}) unavailable: {status}");
                    return HttpFailure(response.StatusCode, response.Headers.RetryAfter);
                }
                var json = await response.Content.ReadAsStringAsync(timeout.Token).ConfigureAwait(false);
                var (report, error) = provider switch
                {
                    AiProviders.Claude => ParseClaude(json, credential.Plan),
                    AiProviders.Codex => ParseCodex(json),
                    AiProviders.OpenCode => ParseOpenCode(json, credential.Plan),
                    AiProviders.Copilot => ParseCopilot(json, credential.Plan),
                    AiProviders.DeepSeek => ParseDeepSeek(json, credential.Plan),
                    AiProviders.OpenRouter => ParseOpenRouter(json, credential.Plan),
                    _ => ParseAntigravity(json, credential.Plan)
                };
                return new(report, error);
            }
            finally { response.Dispose(); }
        }
        catch (Exception ex) when (ex is HttpRequestException or OperationCanceledException or TimeoutException
            or IOException or UnauthorizedAccessException or JsonException or KeyNotFoundException
            or InvalidOperationException or ArgumentException or FormatException or NotSupportedException or SecurityException)
        {
            AppLog.Info($"AI usage ({provider}) unavailable: {status} {ex.GetType().Name}");
            return new(null, "ErrorAiUsageLoad");
        }
    }

    internal static LoadResult HttpFailure(HttpStatusCode status, RetryConditionHeaderValue? retryAfter) => status switch
    {
        HttpStatusCode.Unauthorized => new(null, "ErrorAiUsageSignIn"),
        HttpStatusCode.TooManyRequests => new(null, "ErrorAiUsageRateLimited",
            retryAfter?.Delta ?? (retryAfter?.Date is { } date ? date - DateTimeOffset.UtcNow : null)),
        _ => new(null, "ErrorAiUsageLoad")
    };

    internal static string CredentialPath(AiProviders provider)
    {
        if (provider == AiProviders.Copilot)
            return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "github-copilot", "apps.json");

        var (envVar, folder, file) = provider switch
        {
            AiProviders.Claude => ("CLAUDE_CONFIG_DIR", ".claude", ".credentials.json"),
            AiProviders.Codex => ("CODEX_HOME", ".codex", "auth.json"),
            AiProviders.OpenCode => ("OPENCODE_CONFIG_DIR", Path.Combine(".local", "share", "opencode"), "auth.json"),
            AiProviders.DeepSeek => ("DEEPSEEK_CONFIG_DIR", ".deepseek", "auth.json"),
            AiProviders.OpenRouter => ("OPENROUTER_CONFIG_DIR", ".openrouter", "auth.json"),
            _ => ("GEMINI_CONFIG_DIR", ".gemini", "oauth_creds.json")
        };
        var directory = Environment.GetEnvironmentVariable(envVar);
        if (string.IsNullOrWhiteSpace(directory))
            directory = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), folder);
        return Path.Combine(directory, file);
    }

    internal static DateTime CredentialStamp(AiProviders provider)
    {
        var stamp = DateTime.MinValue;
        if (provider == AiProviders.Antigravity && ReadKeyringCredential("gemini:antigravity", out var keyringStamp) is not null)
            stamp = keyringStamp;
        else if (provider == AiProviders.Claude && ReadKeyringCredential("Claude Code-credentials", out var claudeKeyringStamp) is not null)
            stamp = claudeKeyringStamp;
        try
        {
            var fileStamp = File.GetLastWriteTimeUtc(CredentialPath(provider));
            if (fileStamp > stamp) stamp = fileStamp;
            if (provider == AiProviders.OpenCode)
            {
                var dbPath = Path.Combine(Path.GetDirectoryName(CredentialPath(provider)) ?? string.Empty, "opencode.db");
                var dbStamp = File.GetLastWriteTimeUtc(dbPath);
                if (dbStamp > stamp) stamp = dbStamp;
            }
            if (provider == AiProviders.Copilot)
            {
                var fallbackPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".config", "github-copilot", "apps.json");
                var fallbackStamp = File.GetLastWriteTimeUtc(fallbackPath);
                if (fallbackStamp > stamp) stamp = fallbackStamp;
            }
            return stamp;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or SecurityException) { return stamp; }
    }

    internal static string? ReadClaudeToken(string? path = null) =>
        ReadCredential(AiProviders.Claude, path ?? CredentialPath(AiProviders.Claude)) is { Token.Length: > 0 } cred ? cred.Token : null;
    internal static string? ReadCodexToken(string? path = null) => ReadCredential(AiProviders.Codex, path ?? CredentialPath(AiProviders.Codex))?.Token;
    internal static string? ReadAntigravityToken(string? path = null) => ReadCredential(AiProviders.Antigravity, path)?.Token;
    internal static string? ReadOpenCodeToken(string? path = null) => ReadCredential(AiProviders.OpenCode, path)?.Token;
    internal static string? ReadCopilotToken(string? path = null) => ReadCredential(AiProviders.Copilot, path)?.Token;
    internal static string? ReadDeepSeekToken(string? path = null) => ReadCredential(AiProviders.DeepSeek, path)?.Token;
    internal static string? ReadOpenRouterToken(string? path = null) => ReadCredential(AiProviders.OpenRouter, path)?.Token;

    internal static async Task<string?> TryRefreshClaudeTokenAsync(
        string path,
        Func<string, CancellationToken, Task<string?>>? exchange = null,
        CancellationToken cancellation = default)
    {
        if (ReadCredential(AiProviders.Claude, path) is not { } credential) return null;
        if (!string.IsNullOrWhiteSpace(credential.Token)) return credential.Token;
        return (await RefreshClaudeCredentialAsync(credential, exchange, cancellation).ConfigureAwait(false))?.Token;
    }

    private static async Task<HttpResponseMessage> SendUsageRequestAsync(AiProviders provider, Credential credential, CancellationToken cancellation)
    {
        using var request = new HttpRequestMessage(HttpMethod.Get, provider switch
        {
            AiProviders.Claude => "https://api.anthropic.com/api/oauth/usage",
            AiProviders.Codex => "https://chatgpt.com/backend-api/wham/usage",
            AiProviders.OpenCode => "https://opencode.ai/zen/go/v1/usage",
            AiProviders.Copilot => "https://api.github.com/copilot_internal/user",
            AiProviders.DeepSeek => "https://api.deepseek.com/user/balance",
            AiProviders.OpenRouter => "https://openrouter.ai/api/v1/credits",
            _ => "https://aicode.googleapis.com/v1alpha:fetchQuotaStatus"
        });
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", credential.Token);
        if (provider == AiProviders.Claude) request.Headers.Add("anthropic-beta", "oauth-2025-04-20");
        else if (provider == AiProviders.Antigravity) request.Headers.UserAgent.ParseAdd("antigravity");
        else if (provider == AiProviders.Copilot) request.Headers.TryAddWithoutValidation("Accept", "application/vnd.github+json");
        else if (!string.IsNullOrWhiteSpace(credential.Account)) request.Headers.Add("ChatGPT-Account-Id", credential.Account);
        return await Client.SendAsync(request, cancellation).ConfigureAwait(false);
    }

    private static Credential? ReadCredential(AiProviders provider, string? path = null)
    {
        if (provider == AiProviders.Antigravity && path is null
            && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("GEMINI_CONFIG_DIR"))
            && ReadKeyringCredential("gemini:antigravity", out var unusedLastWritten) is { } keyringJson
            && ParseAntigravityCredential(keyringJson) is { } keyringCred)
            return keyringCred;
        if (provider == AiProviders.OpenCode && path is null)
        {
            foreach (var envKey in new[] { "OPENCODE_GO_API_KEY", "OPENCODE_API_KEY", "OPENCODE_ZEN_API_KEY" })
                if (Environment.GetEnvironmentVariable(envKey) is { Length: > 0 } envToken && !string.IsNullOrWhiteSpace(envToken))
                    return new(envToken.Trim());
        }
        if (provider == AiProviders.DeepSeek && path is null)
        {
            var envToken = Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY")
                ?? Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY", EnvironmentVariableTarget.User)
                ?? Environment.GetEnvironmentVariable("DEEPSEEK_API_KEY", EnvironmentVariableTarget.Machine);
            if (!string.IsNullOrWhiteSpace(envToken))
                return new(envToken.Trim());
        }
        if (provider == AiProviders.OpenRouter && path is null)
        {
            var envToken = Environment.GetEnvironmentVariable("OPENROUTER_API_KEY")
                ?? Environment.GetEnvironmentVariable("OPENROUTER_API_KEY", EnvironmentVariableTarget.User)
                ?? Environment.GetEnvironmentVariable("OPENROUTER_API_KEY", EnvironmentVariableTarget.Machine);
            if (!string.IsNullOrWhiteSpace(envToken))
                return new(envToken.Trim());
        }
        try
        {
            var targetPath = path ?? CredentialPath(provider);
            if (provider == AiProviders.Claude && path is null && !File.Exists(targetPath)
                && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"))
                && ReadKeyringCredential("Claude Code-credentials", out _) is { } claudeKeyringJson
                && ParseClaudeCredential(claudeKeyringJson, null) is { } claudeKeyringCred)
                return claudeKeyringCred;
            if (provider == AiProviders.OpenCode && path is null && !File.Exists(targetPath))
            {
                var legacyPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".opencode", "auth.json");
                if (File.Exists(legacyPath)) targetPath = legacyPath;
                else if (TryReadOpenCodeCliCredential() is { } cliCred) return cliCred;
            }
            if (provider == AiProviders.Copilot && path is null && !File.Exists(targetPath))
            {
                var configPath = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), ".config", "github-copilot", "apps.json");
                if (File.Exists(configPath)) targetPath = configPath;
            }
            using var stream = new FileStream(targetPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
            using var document = JsonDocument.Parse(stream);
            var root = document.RootElement;
            if (provider == AiProviders.Antigravity) return ParseAntigravityCredential(root);
            if (provider == AiProviders.OpenCode) return ParseOpenCodeCredential(root);
            if (provider == AiProviders.Copilot) return ParseCopilotCredential(root);
            if (provider == AiProviders.DeepSeek) return ParseDeepSeekCredential(root);
            if (provider == AiProviders.OpenRouter) return ParseOpenRouterCredential(root);
            if (provider == AiProviders.Claude)
            {
                var fileCred = ParseClaudeCredential(root, targetPath);
                if (fileCred is { Token.Length: > 0 }) return fileCred;
                if (path is null && string.IsNullOrWhiteSpace(Environment.GetEnvironmentVariable("CLAUDE_CONFIG_DIR"))
                    && ReadKeyringCredential("Claude Code-credentials", out _) is { } fallbackKeyringJson
                    && ParseClaudeCredential(fallbackKeyringJson, targetPath) is { } fallbackKeyringCred
                    && (!string.IsNullOrWhiteSpace(fallbackKeyringCred.Token) || fileCred is null))
                    return fallbackKeyringCred;
                return fileCred;
            }
            var auth = root.GetProperty("tokens");
            var token = auth.GetProperty("access_token").GetString();
            if (string.IsNullOrWhiteSpace(token)) return null;
            return new(token, Text(auth, "account_id"));
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException
            or KeyNotFoundException or InvalidOperationException or FormatException or ArgumentException
            or NotSupportedException or SecurityException) { return null; }
    }

    private static Credential? ParseClaudeCredential(string json, string? filePath)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            return ParseClaudeCredential(document.RootElement, filePath);
        }
        catch (JsonException) { return null; }
    }

    private static Credential? ParseClaudeCredential(JsonElement root, string? filePath)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var auth = Property(root, "claudeAiOauth") ?? root;
        if (auth.ValueKind != JsonValueKind.Object) return null;
        var token = Text(auth, "accessToken") ?? Text(auth, "access_token");
        var refresh = Text(auth, "refreshToken") ?? Text(auth, "refresh_token");
        var plan = Text(auth, "subscriptionType") ?? Text(auth, "rateLimitTier");
        var expired = Property(auth, "expiresAt") is { ValueKind: JsonValueKind.Number } expiry
            && expiry.TryGetInt64(out var ms)
            && DateTimeOffset.FromUnixTimeMilliseconds(ms) <= DateTimeOffset.UtcNow;
        var usableToken = !string.IsNullOrWhiteSpace(token) && !expired ? token : string.Empty;
        if (usableToken.Length == 0 && string.IsNullOrWhiteSpace(refresh)) return null;
        return new(usableToken, Plan: plan, RefreshToken: string.IsNullOrWhiteSpace(refresh) ? null : refresh, FilePath: filePath);
    }

    private static async Task<Credential?> RefreshClaudeCredentialAsync(
        Credential credential,
        Func<string, CancellationToken, Task<string?>>? exchange,
        CancellationToken cancellation)
    {
        if (string.IsNullOrWhiteSpace(credential.RefreshToken)) return null;
        try
        {
            string? responseJson;
            if (exchange is not null)
            {
                responseJson = await exchange(credential.RefreshToken, cancellation).ConfigureAwait(false);
            }
            else
            {
                var payload = JsonSerializer.Serialize(new
                {
                    grant_type = "refresh_token",
                    refresh_token = credential.RefreshToken,
                    client_id = ClaudeOAuthClientId
                });
                using var request = new HttpRequestMessage(HttpMethod.Post, "https://api.anthropic.com/v1/oauth/token")
                {
                    Content = new StringContent(payload, Encoding.UTF8, "application/json")
                };
                using var response = await Client.SendAsync(request, cancellation).ConfigureAwait(false);
                if (!response.IsSuccessStatusCode) return null;
                responseJson = await response.Content.ReadAsStringAsync(cancellation).ConfigureAwait(false);
            }
            if (string.IsNullOrWhiteSpace(responseJson)) return null;
            using var document = JsonDocument.Parse(responseJson);
            var root = document.RootElement;
            var newAccess = Text(root, "access_token") ?? Text(root, "accessToken");
            if (string.IsNullOrWhiteSpace(newAccess)) return null;
            var newRefresh = Text(root, "refresh_token") ?? Text(root, "refreshToken") ?? credential.RefreshToken;
            var expiresIn = (Property(root, "expires_in") ?? Property(root, "expiresIn")) is { } exp && Number(exp) is { } sec && sec > 0
                ? sec : 28800;
            var newExpiresAt = DateTimeOffset.UtcNow.AddSeconds(expiresIn).ToUnixTimeMilliseconds();
            if (!string.IsNullOrWhiteSpace(credential.FilePath))
                WriteClaudeCredential(credential.FilePath, newAccess, newRefresh, newExpiresAt);
            return credential with { Token = newAccess, RefreshToken = newRefresh };
        }
        catch (Exception ex) when (ex is HttpRequestException or OperationCanceledException or TimeoutException
            or IOException or UnauthorizedAccessException or JsonException)
        {
            return null;
        }
    }

    private static void WriteClaudeCredential(string path, string accessToken, string refreshToken, long expiresAt)
    {
        try
        {
            if (!File.Exists(path)) return;
            using var existing = JsonDocument.Parse(File.ReadAllBytes(path));
            if (existing.RootElement.ValueKind != JsonValueKind.Object) return;
            var tmpPath = path + ".tmp";
            using (var stream = new FileStream(tmpPath, FileMode.Create, FileAccess.Write, FileShare.None))
            using (var writer = new Utf8JsonWriter(stream, new JsonWriterOptions { Indented = true }))
            {
                writer.WriteStartObject();
                var wroteOauth = false;
                foreach (var prop in existing.RootElement.EnumerateObject())
                {
                    if (prop.Name == "claudeAiOauth" && prop.Value.ValueKind == JsonValueKind.Object)
                    {
                        wroteOauth = true;
                        writer.WritePropertyName("claudeAiOauth");
                        writer.WriteStartObject();
                        writer.WriteString("accessToken", accessToken);
                        writer.WriteString("refreshToken", refreshToken);
                        writer.WriteNumber("expiresAt", expiresAt);
                        foreach (var child in prop.Value.EnumerateObject())
                        {
                            if (child.Name is "accessToken" or "access_token" or "refreshToken" or "refresh_token" or "expiresAt")
                                continue;
                            child.WriteTo(writer);
                        }
                        writer.WriteEndObject();
                    }
                    else prop.WriteTo(writer);
                }
                if (!wroteOauth)
                {
                    writer.WritePropertyName("claudeAiOauth");
                    writer.WriteStartObject();
                    writer.WriteString("accessToken", accessToken);
                    writer.WriteString("refreshToken", refreshToken);
                    writer.WriteNumber("expiresAt", expiresAt);
                    writer.WriteEndObject();
                }
                writer.WriteEndObject();
            }
            File.Move(tmpPath, path, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException) { }
    }

    private static Credential? ParseAntigravityCredential( string json)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            return ParseAntigravityCredential(document.RootElement);
        }
        catch (JsonException) { return null; }
    }

    private static Credential? ParseAntigravityCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var auth = Property(root, "token") ?? root;
        var token = Text(auth, "access_token") ?? Text(auth, "accessToken");
        if (string.IsNullOrWhiteSpace(token)) return null;
        if (Property(auth, "expiry_date") is { ValueKind: JsonValueKind.Number } expiryMs
            && expiryMs.TryGetInt64(out var ms)
            && DateTimeOffset.FromUnixTimeMilliseconds(ms) <= DateTimeOffset.UtcNow)
            return null;
        return new(token, Plan: Text(root, "plan") ?? Text(root, "tier"));
    }

    private static Credential? ParseOpenCodeCredential(JsonElement root)
    {
        static string? ExtractKey(JsonElement element)
        {
            if (element.ValueKind != JsonValueKind.Object) return null;
            var target = Property(element, "value") ?? element;
            return Text(target, "key") ?? Text(target, "apiKey") ?? Text(target, "access_token") ?? Text(target, "accessToken") ?? Text(target, "token");
        }
        if (root.ValueKind == JsonValueKind.Array)
        {
            string? fallback = null;
            foreach (var item in root.EnumerateArray())
            {
                var id = Text(item, "integrationID") ?? Text(item, "id") ?? Text(item, "provider");
                if (id is null || (!id.Equals("opencode-go", StringComparison.OrdinalIgnoreCase)
                    && !id.Equals("opencode", StringComparison.OrdinalIgnoreCase))) continue;
                if (ExtractKey(item) is not { Length: > 0 } key || string.IsNullOrWhiteSpace(key)) continue;
                if (id.Equals("opencode-go", StringComparison.OrdinalIgnoreCase) && Flag(item, "active")) return new(key);
                fallback ??= key;
            }
            return fallback is null ? null : new(fallback);
        }
        if (root.ValueKind == JsonValueKind.Object)
        {
            foreach (var section in new[] { "opencode-go", "opencode", "zen" })
                if (Property(root, section) is { } child && ExtractKey(child) is { Length: > 0 } key && !string.IsNullOrWhiteSpace(key))
                    return new(key, Plan: Text(child, "plan") ?? Text(root, "plan"));
            if (ExtractKey(root) is { Length: > 0 } flatKey && !string.IsNullOrWhiteSpace(flatKey))
                return new(flatKey, Plan: Text(root, "plan"));
        }
        return null;
    }

    private static Credential? TryReadOpenCodeCliCredential()
    {
        try
        {
            var dbPath = Path.Combine(Path.GetDirectoryName(CredentialPath(AiProviders.OpenCode)) ?? string.Empty, "opencode.db");
            if (!File.Exists(dbPath)) return null;
            using var process = new Process
            {
                StartInfo = HiddenCliStartInfo("opencode", "auth", "export")
            };
            if (!process.Start()) return null;
            process.StandardInput.Close();
            AppLog.Info($"AI usage CLI started: OpenCode pid={process.Id}");
            var stdout = process.StandardOutput.ReadToEnd();
            if (!process.WaitForExit(5000) || process.ExitCode != 0 || string.IsNullOrWhiteSpace(stdout)) return null;
            using var document = JsonDocument.Parse(stdout);
            return ParseOpenCodeCredential(document.RootElement);
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or JsonException or InvalidOperationException)
        {
            return null;
        }
    }

    internal static (AiUsageReport? Report, string? Error) ParseClaude(string json, string? plan) => Parse(json, AiProviders.Claude, plan);
    internal static (AiUsageReport? Report, string? Error) ParseCodex(string json) => Parse(json, AiProviders.Codex, null);
    internal static (AiUsageReport? Report, string? Error) ParseAntigravity(string json, string? plan = null) => Parse(json, AiProviders.Antigravity, plan);
    internal static (AiUsageReport? Report, string? Error) ParseOpenCode(string json, string? plan = null) => Parse(json, AiProviders.OpenCode, plan);
    internal static (AiUsageReport? Report, string? Error) ParseCopilot(string json, string? plan = null) => Parse(json, AiProviders.Copilot, plan);
    internal static (AiUsageReport? Report, string? Error) ParseDeepSeek(string json, string? plan = null) => Parse(json, AiProviders.DeepSeek, plan);
    internal static (AiUsageReport? Report, string? Error) ParseOpenRouter(string json, string? plan = null) => Parse(json, AiProviders.OpenRouter, plan);

    private static Credential? ParseCopilotCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        foreach (var entry in root.EnumerateObject())
        {
            if (entry.Value.ValueKind == JsonValueKind.Object
                && Text(entry.Value, "oauth_token") is { Length: > 0 } token)
            {
                return new(token);
            }
        }
        return null;
    }

    private static Credential? ParseDeepSeekCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var token = Text(root, "api_key") ?? Text(root, "key") ?? Text(root, "token");
        return string.IsNullOrWhiteSpace(token) ? null : new(token);
    }

    private static Credential? ParseOpenRouterCredential(JsonElement root)
    {
        if (root.ValueKind != JsonValueKind.Object) return null;
        var token = Text(root, "api_key") ?? Text(root, "key") ?? Text(root, "token");
        return string.IsNullOrWhiteSpace(token) ? null : new(token);
    }

    private static (AiUsageReport? Report, string? Error) Parse(string json, AiProviders provider, string? plan)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            var now = DateTimeOffset.UtcNow;
            var windows = new List<AiUsageWindow>();
            AiUsageReport report;
            if (provider == AiProviders.Claude)
            {
                var severity = new Dictionary<string, string>();
                if (Property(root, "limits") is { ValueKind: JsonValueKind.Array } limits)
                    foreach (var limit in limits.EnumerateArray())
                        if (Text(limit, "group") is { } group && Text(limit, "severity") is { } level) severity.TryAdd(group, level);
                foreach (var property in root.EnumerateObject())
                {
                    if (property.Name != "five_hour" && property.Name != "seven_day"
                        && !property.Name.StartsWith("seven_day_", StringComparison.Ordinal)) continue;
                    var value = property.Value;
                    if (value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("utilization", out var usage)
                        || Percent(usage) is not { } used) continue;
                    var reset = Text(value, "resets_at");
                    var label = property.Name == "five_hour" ? "AiUsageSession" : property.Name == "seven_day"
                        ? "AiUsageWeekly" : "AiUsageWeeklyModel:" + property.Name[10..];
                    windows.Add(new(label, used,
                        DateTimeOffset.TryParse(reset, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var resetsAt) ? resetsAt : null,
                        property.Name == "five_hour" ? 18000 : 604800,
                        property.Name == "five_hour" ? severity.GetValueOrDefault("session")
                            : property.Name == "seven_day" ? severity.GetValueOrDefault("weekly") : null));
                }
                var breakdown = new List<AiUsageShare>();
                if (Property(Property(root, "seven_day_breakdown"), "rows") is { ValueKind: JsonValueKind.Array } rows)
                    foreach (var row in rows.EnumerateArray())
                        if (Text(row, "display_name") is { Length: > 0 } name && Property(row, "percent_of_limit") is { } share
                            && Percent(share) is { } part && Math.Round(part * 100) >= 1) breakdown.Add(new(name, part));
                report = new(provider, PlanLabel(plan), windows, now) { Breakdown = breakdown, Spend = ParseSpend(root) };
            }
            else if (provider == AiProviders.Antigravity)
            {
                plan ??= Text(root, "plan") ?? Text(root, "tier");
                var groups = Property(Property(Property(root, "command"), "data"), "groups")
                    ?? Property(Property(root, "quotaSummary"), "groups")
                    ?? Property(root, "groups");
                if (groups is { ValueKind: JsonValueKind.Array } groupArray)
                {
                    foreach (var group in groupArray.EnumerateArray())
                    {
                        var groupName = NormalizeAntigravityGroup(Text(group, "name") ?? Text(group, "displayName"));
                        if (string.IsNullOrWhiteSpace(groupName) || Property(group, "buckets") is not { ValueKind: JsonValueKind.Array } groupBuckets)
                            continue;
                        var groupModels = ExtractGroupModels(Text(group, "description"));
                        foreach (var bucket in groupBuckets.EnumerateArray())
                        {
                            var resetText = Text(bucket, "reset_time") ?? Text(bucket, "resetTime");
                            DateTimeOffset? resetAt = resetText is not null
                                && DateTimeOffset.TryParse(resetText, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var parsed)
                                ? parsed : null;
                            double used;
                            if ((Property(bucket, "remaining_fraction") ?? Property(bucket, "remainingFraction")) is { } rem)
                            {
                                if (Number(rem) is not { } fraction || !double.IsFinite(fraction)) continue;
                                used = Math.Clamp(1.0 - fraction, 0.0, 1.0);
                            }
                            else if (resetAt is not null) used = 1.0;
                            else continue;
                            var windowId = Text(bucket, "window") ?? Text(bucket, "refreshWindowLabel") ?? Text(bucket, "id") ?? Text(bucket, "bucketId");
                            var isFiveHour = windowId is not null
                                && (windowId.Equals("5h", StringComparison.OrdinalIgnoreCase) || windowId.EndsWith("-5h", StringComparison.OrdinalIgnoreCase));
                            windows.Add(new(
                                isFiveHour ? "AiUsageSession" : "AiUsageWeekly",
                                used,
                                resetAt,
                                isFiveHour ? 18000L : 604800L,
                                Group: groupName,
                                GroupModels: groupModels));
                        }
                    }
                }
                else
                {
                    var pools = new Dictionary<string, (double Used, DateTimeOffset? ResetsAt)>(StringComparer.Ordinal);
                    void CollectBucket(string? modelId, JsonElement quota)
                    {
                        if (AntigravityPool(modelId) is not { } pool || quota.ValueKind != JsonValueKind.Object) return;
                        DateTimeOffset? resetAt = Text(quota, "resetTime") is { } resetText
                            && DateTimeOffset.TryParse(resetText, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var parsed)
                            ? parsed : null;
                        double used;
                        if (Property(quota, "remainingFraction") is { } rem)
                        {
                            if (Number(rem) is not { } fraction || !double.IsFinite(fraction)) return;
                            used = Math.Clamp(1.0 - fraction, 0.0, 1.0);
                        }
                        else if (resetAt is not null) used = 1.0;
                        else return;
                        if (!pools.TryGetValue(pool, out var existing) || used > existing.Used
                            || (Math.Abs(used - existing.Used) < 1e-6 && existing.ResetsAt is null && resetAt is not null))
                            pools[pool] = (used, resetAt ?? existing.ResetsAt);
                    }
                    if (Property(root, "buckets") is { ValueKind: JsonValueKind.Array } buckets)
                        foreach (var bucket in buckets.EnumerateArray())
                            CollectBucket(Text(bucket, "modelId"), bucket);
                    if (Property(root, "models") is { ValueKind: JsonValueKind.Object } models)
                        foreach (var prop in models.EnumerateObject())
                            if (Property(prop.Value, "quotaInfo") is { } quotaInfo)
                                CollectBucket(prop.Name, quotaInfo);
                    // When GPT-OSS shares the exact same partner quota pool as Claude, avoid repeating an identical row.
                    if (pools.TryGetValue("Claude", out var claudePool) && pools.TryGetValue("GPT-OSS", out var gptPool)
                        && Math.Abs(claudePool.Used - gptPool.Used) < 1e-4 && claudePool.ResetsAt == gptPool.ResetsAt)
                        pools.Remove("GPT-OSS");
                    foreach (var name in new[] { "Gemini Pro", "Gemini Flash", "Claude", "GPT-OSS" })
                    {
                        if (!pools.TryGetValue(name, out var pool)) continue;
                        long windowSeconds = pool.ResetsAt is { } r && (r - now).TotalSeconds > 18000 ? 604800L : 18000L;
                        windows.Add(new("AiUsagePool:" + name, pool.Used, pool.ResetsAt, windowSeconds));
                    }
                }
                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = windows.Count > 0
                        && windows.GroupBy(window => window.Group ?? window.LabelKey, StringComparer.Ordinal)
                            .All(group => group.Any(window => window.Used >= 1.0))
                };
            }
            else if (provider == AiProviders.OpenCode)
            {
                plan ??= Text(root, "plan") ?? Text(root, "plan_type") ?? Text(root, "tier") ?? "Go";
                (string[] Keys, string LabelKey, long Seconds)[] specs =
                [
                    (["rolling5h", "rollingUsage", "five_hour", "fiveHour"], "AiUsageSession", 18000L),
                    (["weekly", "weeklyUsage", "seven_day"], "AiUsageWeekly", 604800L),
                    (["monthly", "monthlyUsage", "thirty_day"], "AiUsageMonthly", 2592000L)
                ];
                foreach (var (keys, labelKey, defaultSeconds) in specs)
                {
                    JsonElement? slot = null;
                    foreach (var key in keys)
                        if (Property(root, key) is { ValueKind: JsonValueKind.Object } found) { slot = found; break; }
                    if (slot is not { } value) continue;
                    double? used = null;
                    foreach (var pctKey in new[] { "usagePercent", "used_percent", "utilization", "percent" })
                        if (Property(value, pctKey) is { } pct && Percent(pct) is { } parsedPct) { used = parsedPct; break; }
                    if (used is null
                        && (Property(value, "usageDollars") ?? Property(value, "used")) is { } usedElem && Number(usedElem) is { } usedAmt
                        && (Property(value, "limitDollars") ?? Property(value, "limit")) is { } limitElem && Number(limitElem) is { } limitAmt
                        && double.IsFinite(usedAmt) && double.IsFinite(limitAmt) && limitAmt > 0)
                        used = Math.Clamp(usedAmt / limitAmt, 0.0, 1.0);
                    if (used is not { } finalUsed) continue;
                    DateTimeOffset? resetAt = null;
                    foreach (var absKey in new[] { "resetsAt", "reset_at", "resetTime" })
                    {
                        if (Property(value, absKey) is not { } abs) continue;
                        if (abs.ValueKind == JsonValueKind.String
                            && DateTimeOffset.TryParse(abs.GetString(), CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var isoReset))
                        { resetAt = isoReset; break; }
                        if (UnixReset(abs) is { } unixReset) { resetAt = unixReset; break; }
                    }
                    if (resetAt is null)
                    {
                        foreach (var relKey in new[] { "resetInSec", "reset_after_seconds", "resetInSeconds" })
                            if (Property(value, relKey) is { } rel && Number(rel) is { } after && after is >= 0 and < MaxResetSeconds)
                            { resetAt = now.AddSeconds(after); break; }
                    }
                    windows.Add(new(labelKey, finalUsed, resetAt, defaultSeconds));
                }
                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = Flag(root, "limit_reached") || Flag(root, "limitReached")
                        || windows.Any(window => window.Used >= 1.0)
                };
            }
            else if (provider == AiProviders.Copilot)
            {
                plan ??= Text(root, "copilot_plan");
                DateTimeOffset? resetAt = null;
                if (Text(root, "quota_reset_date_utc") is { } resetRaw
                    && DateTimeOffset.TryParse(resetRaw, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var parsedReset))
                {
                    resetAt = parsedReset;
                }

                if (Property(root, "quota_snapshots") is { ValueKind: JsonValueKind.Object } snapshots
                    && Property(snapshots, "premium_interactions") is { ValueKind: JsonValueKind.Object } premium
                    && Property(premium, "percent_remaining") is { } remElem
                    && Number(remElem) is { } percentRemaining
                    && double.IsFinite(percentRemaining))
                {
                    var used = Math.Clamp((100.0 - percentRemaining) / 100.0, 0.0, 1.0);
                    windows.Add(new("AiUsageMonthly", used, resetAt, 2592000L));
                }

                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = windows.Any(window => window.Used >= 1.0)
                };
            }
            else if (provider == AiProviders.DeepSeek)
            {
                var isAvailable = Flag(root, "is_available");
                double? totalBalance = null;
                string? currency = null;
                if (Property(root, "balance_infos") is { ValueKind: JsonValueKind.Array } infos)
                {
                    foreach (var info in infos.EnumerateArray())
                    {
                        if (info.ValueKind != JsonValueKind.Object) continue;
                        var totalText = Text(info, "total_balance");
                        if (double.TryParse(totalText, NumberStyles.Float, CultureInfo.InvariantCulture, out var parsedBalance))
                        {
                            totalBalance = parsedBalance;
                            currency = Text(info, "currency") ?? "USD";
                            break;
                        }
                    }
                }

                if (totalBalance is not { } bal)
                    return (null, "ErrorAiUsageLoad");

                plan ??= $"{bal.ToString("0.##", CultureInfo.InvariantCulture)} {currency}";
                windows.Add(new("AiUsageBalance", bal <= 0 ? 1.0 : 0.0, null, null));
                report = new(provider, plan, windows, now)
                {
                    Credits = bal,
                    LimitReached = !isAvailable || bal <= 0
                };
            }
            else if (provider == AiProviders.OpenRouter)
            {
                if (Property(root, "data") is not { ValueKind: JsonValueKind.Object } data
                    || Property(data, "total_credits") is not { } totalElem || Number(totalElem) is not { } totalCredits
                    || Property(data, "total_usage") is not { } usedElem || Number(usedElem) is not { } totalUsage
                    || !double.IsFinite(totalCredits) || !double.IsFinite(totalUsage))
                {
                    return (null, "ErrorAiUsageLoad");
                }

                var remaining = Math.Max(0.0, totalCredits - totalUsage);
                var used = totalCredits > 0 ? Math.Clamp(totalUsage / totalCredits, 0.0, 1.0) : 0.0;
                plan ??= $"${remaining.ToString("0.##", CultureInfo.InvariantCulture)}";
                windows.Add(new("AiUsageCreditsLabel", used, null, null));
                report = new(provider, plan, windows, now)
                {
                    Credits = remaining,
                    LimitReached = totalCredits > 0 && remaining <= 0
                };
            }
            else
            {
                plan = Text(root, "plan_type");
                var limits = root.GetProperty("rate_limit");
                foreach (var name in new[] { "primary_window", "secondary_window" })
                {
                    if (limits.ValueKind != JsonValueKind.Object || !limits.TryGetProperty(name, out var value)
                        || value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("used_percent", out var usage)
                        || Percent(usage) is not { } used) continue;
                    long? seconds = Property(value, "limit_window_seconds") is { } duration && Number(duration) is { } length
                        && length is > 0 and < 1e9 ? (long)length : null;
                    // An unusable absolute reset falls back to the relative one; out-of-range values drop only the reset.
                    var reset = (Property(value, "reset_at") is { } absolute ? UnixReset(absolute) : null)
                        ?? (Property(value, "reset_after_seconds") is { } relative && Number(relative) is { } after
                            && after is >= 0 and < MaxResetSeconds ? (DateTimeOffset?)now.AddSeconds(after) : null);
                    var weekly = seconds is { } span ? span >= 604800 : name == "secondary_window";
                    windows.Add(new(weekly ? "AiUsageWeekly" : "AiUsageSession", used, reset, seconds));
                }
                var credits = Property(root, "credits");
                var unlimited = Flag(credits, "unlimited");
                report = new(provider, PlanLabel(plan), windows, now)
                {
                    LimitReached = Flag(limits, "limit_reached") || Property(limits, "allowed") is { ValueKind: JsonValueKind.False },
                    UnlimitedCredits = unlimited,
                    Credits = !unlimited && Flag(credits, "has_credits") && Property(credits, "balance") is { } balance
                        && Number(balance) is { } amount && double.IsFinite(amount) ? amount : null,
                    // Only resets usable right now count; available_count also includes ones that do not apply yet.
                    ResetCredits = Property(Property(root, "rate_limit_reset_credits"), "applicable_available_count") is { } count
                        && Number(count) is { } available && available is > 0 and < 1000 ? (int)available : 0
                };
            }
            return windows.Count == 0 ? (null, "ErrorAiUsageLoad") : (report, null);
        }
        catch (Exception ex) when (ex is JsonException or KeyNotFoundException or InvalidOperationException
            or FormatException or ArgumentException or OverflowException) { return (null, "ErrorAiUsageLoad"); }
    }

    /// <summary>Claude pay-as-you-go spend, only when the account has it switched on.</summary>
    private static AiUsageSpend? ParseSpend(JsonElement root)
    {
        var spend = Property(root, "spend");
        if (!Flag(spend, "enabled") && !Flag(Property(root, "extra_usage"), "is_enabled")) return null;
        if (Money(Property(spend, "used")) is not { } used) return null;
        var limit = Money(Property(spend, "limit"));
        var percent = Property(spend, "percent") is { } value ? Percent(value) : null;
        return new(used.Amount, limit?.Amount, used.Currency, percent);
    }

    private static (decimal Amount, string Currency)? Money(JsonElement? value)
    {
        if (value is not { } money || Property(money, "amount_minor") is not { } minor || Number(minor) is not { } amount
            || !double.IsFinite(amount) || Math.Abs(amount) > 1e12) return null;
        var exponent = Property(money, "exponent") is { } digits && Number(digits) is { } places && places is >= 0 and <= 6 ? (int)places : 2;
        var currency = Text(money, "currency") is { Length: 3 } code ? code : "USD";
        return ((decimal)amount / (decimal)Math.Pow(10, exponent), currency);
    }

    private static JsonElement? Property(JsonElement? element, string key) => element is { ValueKind: JsonValueKind.Object } value
        && value.TryGetProperty(key, out var result) && result.ValueKind != JsonValueKind.Null ? result : null;

    private static bool Flag(JsonElement? element, string key) => Property(element, key) is { ValueKind: JsonValueKind.True };

    private static string? Text(JsonElement element, string key) => element.ValueKind == JsonValueKind.Object
        && element.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static double? Number(JsonElement value) => value.ValueKind == JsonValueKind.Number ? value.GetDouble()
        : value.ValueKind == JsonValueKind.String && double.TryParse(value.GetString(), NumberStyles.Float,
            CultureInfo.InvariantCulture, out var number) ? number : null;

    private static DateTimeOffset? UnixReset(JsonElement value)
    {
        if (Number(value) is not { } number || !double.IsFinite(number)) return null;
        var seconds = number > 1e12 ? number / 1000 : number;
        return seconds is > 0 and < MaxResetSeconds ? DateTimeOffset.UnixEpoch.AddSeconds(seconds) : null;
    }

    internal static string? PlanLabel(string? plan)
    {
        if (string.IsNullOrWhiteSpace(plan)) return null;
        var words = plan.Trim().Split('_', StringSplitOptions.RemoveEmptyEntries);
        plan = words.Length > 2 && words[0] == "default" && words[1] == "claude"
            ? words[2] : string.Join(' ', words);
        if (plan.Length == 0) return null;
        plan = plan[..Math.Min(24, plan.Length)];
        return char.ToUpperInvariant(plan[0]) + plan[1..];
    }

    private static double? Percent(JsonElement value)
    {
        if (Number(value) is not { } number) return null;
        return double.IsFinite(number) ? Math.Clamp(number / 100, 0, 1) : 0;
    }

    private static HttpClient CreateClient()
    {
        var client = new HttpClient(new SocketsHttpHandler { UseCookies = false, AllowAutoRedirect = false,
            PooledConnectionLifetime = TimeSpan.FromMinutes(10) })
        { Timeout = TimeSpan.FromSeconds(15), MaxResponseContentBufferSize = 1 << 20 };
        client.DefaultRequestHeaders.UserAgent.ParseAdd("OpenTaskbar");
        return client;
    }

    private static string? AntigravityPool(string? modelId)
    {
        if (string.IsNullOrWhiteSpace(modelId)) return null;
        if (modelId.StartsWith("gemini-", StringComparison.OrdinalIgnoreCase))
            return modelId.Contains("pro", StringComparison.OrdinalIgnoreCase)
                || modelId.Contains("argon", StringComparison.OrdinalIgnoreCase)
                || modelId.Contains("hydrogen", StringComparison.OrdinalIgnoreCase)
                || modelId.Contains("ultra", StringComparison.OrdinalIgnoreCase)
                ? "Gemini Pro" : "Gemini Flash";
        if (modelId.StartsWith("claude-", StringComparison.OrdinalIgnoreCase)) return "Claude";
        if (modelId.StartsWith("gpt-", StringComparison.OrdinalIgnoreCase)) return "GPT-OSS";
        return null;
    }

    private static string? NormalizeAntigravityGroup(string? name)
    {
        if (string.IsNullOrWhiteSpace(name)) return null;
        var trimmed = name.Trim();
        if (trimmed.Equals("Gemini Models", StringComparison.OrdinalIgnoreCase)) return "AiUsageGroupGemini";
        if (trimmed.Equals("Claude and GPT models", StringComparison.OrdinalIgnoreCase)) return "AiUsageGroupClaudeGpt";
        return trimmed;
    }

    private static string? ExtractGroupModels(string? description)
    {
        if (string.IsNullOrWhiteSpace(description)) return null;
        const string prefix = "Models within this group:";
        return description.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)
            ? description[prefix.Length..].Trim()
            : null;
    }

    private static async Task<AiUsageReport?> TryLoadAntigravityCliAsync(string? plan, CancellationToken cancellation)
    {
        try
        {
            using var process = new Process
            {
                StartInfo = HiddenCliStartInfo("agy", "-p", "/usage", "--output-format", "json")
            };
            if (!process.Start()) return null;
            process.StandardInput.Close();
            AppLog.Info($"AI usage CLI started: Antigravity pid={process.Id}");
            var stdoutTask = process.StandardOutput.ReadToEndAsync(cancellation);
            var stderrTask = process.StandardError.ReadToEndAsync(cancellation);
            try
            {
                await process.WaitForExitAsync(cancellation).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                try { process.Kill(entireProcessTree: true); } catch (InvalidOperationException) { }
                throw;
            }
            var stdout = await stdoutTask.ConfigureAwait(false);
            _ = await stderrTask.ConfigureAwait(false);
            if (process.ExitCode != 0 || string.IsNullOrWhiteSpace(stdout)) return null;
            return ParseAntigravity(stdout, plan).Report;
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or IOException or InvalidOperationException)
        {
            return null;
        }
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NativeCredential
    {
        public uint Flags;
        public uint Type;
        public nint TargetName;
        public nint Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint CredentialBlobSize;
        public nint CredentialBlob;
        public uint Persist;
        public uint AttributeCount;
        public nint Attributes;
        public nint TargetAlias;
        public nint UserName;
    }

    [DllImport("advapi32.dll", EntryPoint = "CredReadW", SetLastError = true, CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CredRead(string target, uint type, uint reservedFlag, out nint credentialPtr);

    [DllImport("advapi32.dll", SetLastError = true)]
    private static extern void CredFree(nint credentialPtr);

    private static string? ReadKeyringCredential(string target, out DateTime lastWrittenUtc)
    {
        lastWrittenUtc = DateTime.MinValue;
        if (!CredRead(target, 1, 0, out var ptr) || ptr == 0) return null;
        try
        {
            var cred = Marshal.PtrToStructure<NativeCredential>(ptr);
            var fileTime = ((long)cred.LastWritten.dwHighDateTime << 32) | (uint)cred.LastWritten.dwLowDateTime;
            if (fileTime > 0) lastWrittenUtc = DateTime.FromFileTimeUtc(fileTime);
            if (cred.CredentialBlob == 0 || cred.CredentialBlobSize is 0 or > (1 << 16)) return null;
            var bytes = new byte[cred.CredentialBlobSize];
            Marshal.Copy(cred.CredentialBlob, bytes, 0, bytes.Length);
            return Encoding.UTF8.GetString(bytes);
        }
        catch (Exception ex) when (ex is ArgumentException or OverflowException or SecurityException) { return null; }
        finally { CredFree(ptr); }
    }
}
