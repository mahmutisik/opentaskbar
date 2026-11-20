using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Http.Headers;
using System.Security.Cryptography;
using System.Text.Json;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class AiUsageChecks
{
    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        foreach (var command in new[] { "opencode", "agy" })
        {
            var startInfo = AiUsageService.HiddenCliStartInfo(command, "--test");
            Check(startInfo.FileName == command && startInfo.ArgumentList.SequenceEqual(["--test"])
                && !startInfo.UseShellExecute && startInfo.CreateNoWindow
                && startInfo.WindowStyle == ProcessWindowStyle.Hidden
                && startInfo.RedirectStandardInput && startInfo.RedirectStandardOutput
                && startInfo.RedirectStandardError,
                "Quota CLI processes must not create visible or interactive console windows.");
        }

        var claude = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":62,"resets_at":"2026-11-19T12:00:00Z"},"seven_day":{"utilization":31}}
            """, "max");
        Check(claude.Error is null && claude.Report is { Plan: "Max", Windows.Count: 2 }
            && claude.Report.Windows[0].Used == 0.62 && claude.Report.Windows[1].Used == 0.31
            && claude.Report.Windows[0].ResetsAt == DateTimeOffset.Parse("2026-11-19T12:00:00Z"),
            "Claude fixtures must preserve percentages, plan and ISO reset dates.");
        var models = AiUsageService.ParseClaude("""
            {"seven_day_xyz":{"utilization":120},"seven_day_other":{"utilization":-5},"seven_day_nan":{"utilization":"NaN"},"extra_usage":{"utilization":40}}
            """, null).Report!;
        Check(models.Windows.Count == 3 && models.Windows[0].LabelKey == "AiUsageWeeklyModel:xyz",
            "Unknown weekly windows must survive; extra usage is not a quota window.");
        Check(models.Windows.Select(window => window.Used).SequenceEqual([1.0, 0.0, 0.0]),
            "Usage must clamp out-of-range and non-finite percentages.");
        var before = DateTimeOffset.UtcNow;
        var codex = AiUsageService.ParseCodex("""
            {"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":18,"reset_at":1800000000,"limit_window_seconds":18000},"secondary_window":{"used_percent":7,"reset_after_seconds":60,"limit_window_seconds":604800}}}
            """).Report!;
        Check(codex.Plan == "Plus" && codex.Windows[0].Used == 0.18 && codex.Windows[0].LabelKey == "AiUsageSession"
            && codex.Windows[0].WindowSeconds == 18000 && codex.Windows[0].ResetsAt == DateTimeOffset.FromUnixTimeSeconds(1800000000),
            "Codex must parse primary usage, duration and Unix reset time.");
        Check(codex.Windows[1].LabelKey == "AiUsageWeekly" && codex.Windows[1].ResetsAt >= before.AddSeconds(60)
            && codex.Windows[1].ResetsAt <= DateTimeOffset.UtcNow.AddSeconds(60), "Codex must support relative reset times.");
        foreach (var invalid in new[] { "null", "true", "[]", "42", "{}", "{\"utilization\":null}", "{\"utilization\":true}",
            "{\"utilization\":\"invalid\"}", "{\"utilization\":{}}", "{\"utilization\":[]}" })
        {
            var partial = AiUsageService.ParseClaude("{\"five_hour\":" + invalid + ",\"seven_day\":{\"utilization\":31}}", null);
            Check(partial.Error is null && partial.Report is { Windows.Count: 1 } && partial.Report.Windows[0].Used == 0.31,
                "A malformed Claude window must leave valid windows available.");
            var partialCodex = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":"
                + invalid.Replace("utilization", "used_percent") + ",\"secondary_window\":{\"used_percent\":31}}}");
            Check(partialCodex.Error is null && partialCodex.Report is { Windows.Count: 1 },
                "A malformed Codex window must leave valid windows available.");
        }
        var unzoned = AiUsageService.ParseClaude("""{"five_hour":{"utilization":1,"resets_at":"2026-11-19T12:00:00"}}""", null);
        Check(unzoned.Report!.Windows[0].ResetsAt == new DateTimeOffset(2026, 11, 19, 12, 0, 0, TimeSpan.Zero),
            "ISO reset dates without an offset must assume UTC.");
        foreach (var reset in new[] { "1800000000.5", "\"1800000000.5\"", "1800000000500", "\"1800000000500\"" })
        {
            var absolute = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":{\"used_percent\":1,\"reset_at\":" + reset + "}}}");
            Check(absolute.Report!.Windows[0].ResetsAt == DateTimeOffset.UnixEpoch.AddSeconds(1800000000.5),
                "Codex reset times must accept fractional seconds and milliseconds as numbers or numeric strings.");
        }
        Check(AiUsageService.PlanLabel("default_claude_max_5x") == "Max"
            && AiUsageService.PlanLabel("plus") == "Plus" && AiUsageService.PlanLabel(new string('x', 40))!.Length == 24,
            "Plan headings must use short, capitalized labels rather than raw API tiers.");
        var full = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":42.0,"resets_at":"2026-11-18T22:00:00Z","limit_dollars":null},
             "seven_day":{"utilization":29.0,"resets_at":"2026-11-24T07:59:00Z"},"seven_day_opus":null,"seven_day_sonnet":null,
             "extra_usage":{"is_enabled":false,"monthly_limit":null,"used_credits":null,"utilization":null},
             "limits":[{"kind":"session","group":"session","percent":42,"severity":"warning"},
                       {"kind":"weekly_all","group":"weekly","percent":29,"severity":"normal"},{"group":null},null,42],
             "spend":{"used":{"amount_minor":0,"currency":"USD","exponent":2},"limit":null,"percent":0,"enabled":false},
             "seven_day_breakdown":{"rows":[{"key":"claude_code","display_name":"Claude Code","percent":98,"percent_of_limit":29},
                {"key":"chat","display_name":"Chats","percent":1,"percent_of_limit":0},{"display_name":null,"percent_of_limit":5},
                {"display_name":"Cowork","percent_of_limit":null},{"display_name":"Other","percent_of_limit":"bad"},
                {"display_name":"Tiny","percent_of_limit":0.4},{"display_name":"Small","percent_of_limit":0.6}]},"codename_x":null}
            """, null).Report!;
        Check(full.Windows.Count == 2 && full.Windows[0].Severity == "warning" && full.Windows[1].Severity == "normal",
            "Claude limit severities must attach to the session and weekly windows.");
        Check(full.Breakdown.Count == 2 && full.Breakdown[0] == new AiUsageShare("Claude Code", 0.29) && full.Breakdown[1].Name == "Small",
            "The weekly breakdown must keep only named rows that round to at least 1% of the limit.");
        Check(full.Spend is null && !full.LimitReached, "Disabled extra usage must not be shown.");
        var spend = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":10},"spend":{"used":{"amount_minor":420,"currency":"EUR","exponent":2},
             "limit":{"amount_minor":5000,"currency":"EUR","exponent":2},"percent":8.4,"enabled":true}}
            """, null).Report!.Spend;
        Check(spend is { Used: 4.2m, Limit: 50m, Currency: "EUR" } && Math.Abs(spend.Used01!.Value - 0.084) < 1e-9,
            "Enabled extra usage must convert minor units and keep the currency.");
        Check(AiUsageService.ParseClaude("""{"five_hour":{"utilization":1},"extra_usage":{"is_enabled":true},"spend":{"used":null}}""", null)
            .Report!.Spend is null, "Extra usage without an amount must stay hidden.");
        var nulls = AiUsageService.ParseClaude("""
            {"five_hour":{"utilization":5},"limits":null,"spend":null,"extra_usage":null,"seven_day_breakdown":{"rows":null}}
            """, null).Report!;
        Check(nulls.Breakdown.Count == 0 && nulls.Spend is null && nulls.Windows[0].Severity is null,
            "Null Claude extras must leave the quota windows usable.");

        var richCodex = AiUsageService.ParseCodex("""
            {"plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":true,
              "primary_window":{"used_percent":31,"limit_window_seconds":18000,"reset_after_seconds":14000,"reset_at":null},
              "secondary_window":{"used_percent":90,"limit_window_seconds":null,"reset_at":1795100000}},
             "code_review_rate_limit":null,"additional_rate_limits":null,
             "credits":{"has_credits":true,"unlimited":false,"balance":"12.5"},
             "rate_limit_reset_credits":{"available_count":3,"applicable_available_count":0},
             "user_id":"fixture-user","account_id":"fixture-account","email":"fixture@example.invalid"}
            """).Report!;
        Check(richCodex.LimitReached && richCodex.Credits == 12.5 && !richCodex.UnlimitedCredits && richCodex.ResetCredits == 0
            && richCodex.Windows.Count == 2 && richCodex.Windows[1] is { WindowSeconds: null, LabelKey: "AiUsageWeekly" },
            "Codex must parse limit state and credits, count only applicable resets, and tolerate a missing window length.");
        Check(AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"rate_limit_reset_credits":{"available_count":3,"applicable_available_count":2}}
            """).Report!.ResetCredits == 2, "Codex must show the number of resets that apply now.");
        var badResets = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":10,"reset_at":1e15,"reset_after_seconds":1e20},
              "secondary_window":{"used_percent":20,"reset_at":"soon","reset_after_seconds":60},
              "extra_window":{"used_percent":1}}}
            """);
        Check(badResets.Error is null && badResets.Report!.Windows[0].ResetsAt is null
            && badResets.Report.Windows[1].ResetsAt is { } fallback && fallback > DateTimeOffset.UtcNow.AddSeconds(30)
            && fallback < DateTimeOffset.UtcNow.AddSeconds(90),
            "Out-of-range resets must drop only the reset, and an unusable absolute reset must fall back to the relative one.");
        Check(AiUsageService.ParseCodex("""{"rate_limit":{"primary_window":{"used_percent":1,"reset_at":-5,"reset_after_seconds":-5}}}""")
            .Report!.Windows[0].ResetsAt is null, "Negative resets must be ignored.");
        Check(AiUsageService.ParseCodex("""{"rate_limit":{"allowed":false,"primary_window":{"used_percent":1}}}""").Report!.LimitReached,
            "A disallowed Codex request must count as a reached limit.");
        var quietCodex = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"credits":{"has_credits":false,"unlimited":false,"balance":"0"},
             "rate_limit_reset_credits":{"available_count":0}}
            """).Report!;
        Check(!quietCodex.LimitReached && quietCodex.Credits is null && quietCodex.ResetCredits == 0,
            "Codex extras must stay hidden when the account has no credits or resets.");
        var unlimited = AiUsageService.ParseCodex("""
            {"rate_limit":{"primary_window":{"used_percent":1}},"credits":{"has_credits":true,"unlimited":true,"balance":null},
             "rate_limit_reset_credits":null}
            """).Report!;
        Check(unlimited.UnlimitedCredits && unlimited.Credits is null && unlimited.ResetCredits == 0,
            "Unlimited Codex credits must not show a balance.");
        foreach (var odd in new[] { "null", "true", "[]", "{\"balance\":\"x\",\"has_credits\":true}" })
        {
            var oddCodex = AiUsageService.ParseCodex("{\"rate_limit\":{\"primary_window\":{\"used_percent\":1}},\"credits\":" + odd
                + ",\"rate_limit_reset_credits\":" + odd + "}");
            Check(oddCodex.Error is null && oddCodex.Report is { Credits: null, ResetCredits: 0, LimitReached: false },
                "Malformed Codex extras must not break quota parsing.");
        }

        var now = new DateTimeOffset(2026, 11, 18, 12, 0, 0, TimeSpan.Zero);
        AiUsageWindow Slot(double used, double remainingHours, long? length = 18000) =>
            new("AiUsageSession", used, now.AddHours(remainingHours), length);
        Check(AiUsageWidget.Elapsed(Slot(0.5, 2.5), now) == 0.5, "Half of a five-hour window must have elapsed after 2.5 hours.");
        Check(AiUsageWidget.Elapsed(Slot(0.5, 0), now) is null && AiUsageWidget.Elapsed(Slot(0.5, 6), now) is null
            && AiUsageWidget.Elapsed(Slot(0.5, 1, null), now) is null && AiUsageWidget.Elapsed(Slot(0.5, 1, 0), now) is null
            && AiUsageWidget.Elapsed(new("AiUsageSession", 0.5, null, 18000), now) is null,
            "Pace needs a known window length and a reset inside that window.");
        Check(AiUsageWidget.LimitIn(Slot(0.8, 2.5), now) is { } soon && Math.Abs(soon.TotalMinutes - 37.5) < 0.01,
            "80% used halfway through must reach the limit in 37.5 minutes at the same rate.");
        Check(AiUsageWidget.LimitIn(Slot(0.5, 2.5), now) is null && AiUsageWidget.LimitIn(Slot(0.3, 2.5), now) is null,
            "On or under pace must not warn.");
        Check(AiUsageWidget.LimitIn(Slot(0.2, 4.9), now) is null, "The first minutes of a window must not project a limit.");
        Check(AiUsageWidget.LimitIn(Slot(1, 2.5), now) is null && AiUsageWidget.LimitIn(Slot(0, 2.5), now) is null,
            "Full or unused windows must not project a limit.");
        Check(AiUsageWidget.LimitIn(Slot(0.999, 2.5), now) == TimeSpan.FromMinutes(1),
            "Projected limits must not show less than a minute.");
        Check(AiUsageWidget.LimitIn(new("AiUsageWeekly", 0.6, now.AddDays(3.5), 604800), now) is { } weekly
            && weekly > TimeSpan.FromDays(1) && weekly < TimeSpan.FromDays(3.5), "Weekly windows must project past a day.");

        var ringReport = new AiUsageReport(AiProviders.Claude, null,
            [new("AiUsageSession", 0.2, null, 18000), new("AiUsageWeekly", 0.4, null, 604800), new("AiUsageWeeklyModel:opus", 0.7, null, 604800)],
            now);
        Check(AiUsageWidget.RingWindow(ringReport, AiUsageRing.Highest)!.Used == 0.4
            && AiUsageWidget.RingWindow(ringReport, AiUsageRing.Session)!.Used == 0.2
            && AiUsageWidget.RingWindow(ringReport, AiUsageRing.Weekly)!.Used == 0.4,
            "The ring must follow the selected window; Highest ignores per-model weekly windows.");
        Check(AiUsageWidget.RingWindow(ringReport with { Windows = [new("AiUsageWeeklyModel:opus", 0.7, null, 604800)] },
            AiUsageRing.Highest)!.Used == 0.7, "Per-model windows must still show when nothing else is reported.");
        var sessionOnly = ringReport with { Windows = [new("AiUsageSession", 0.2, null, 18000)] };
        Check(AiUsageWidget.RingWindow(sessionOnly, AiUsageRing.Weekly)!.Used == 0.2, "A missing selected window must fall back to the highest.");
        Check(AiUsageWidget.Accent(0.5) == "WidgetAccent" && AiUsageWidget.Accent(0.75) == "WidgetWarning" && AiUsageWidget.Accent(0.9) == "WidgetFocusAccent"
            && AiUsageWidget.Accent(0.2, "warning") == "WidgetWarning" && AiUsageWidget.Accent(0.2, "critical") == "WidgetFocusAccent"
            && AiUsageWidget.Accent(0.95, "normal") == "WidgetFocusAccent" && AiUsageWidget.Accent(0.2, "unknown") == "WidgetAccent",
            "Provider severity may raise the accent but never hide high usage.");
        AiUsageReport Report(params AiUsageWindow[] windows) => new(AiProviders.Codex, null, windows, now);
        Check(AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.31, null, 18000), new("AiUsageWeekly", 0.9, null, 604800)))
                == ("AiUsageStatusNearLimit", "WidgetFocusAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.8, null, 18000))) == ("AiUsageStatusHigh", "WidgetWarning")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.2, null, 18000, "warning"))) == ("AiUsageStatusHigh", "WidgetWarning")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.2, null, 18000), new("AiUsageWeeklyModel:opus", 0.95, null, 604800)))
                == ("AiUsageStatusOk", "WidgetAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageSession", 0.5, null, 18000)) with { LimitReached = true })
                == ("AiUsageLimitReached", "WidgetFocusAccent")
            && AiUsageWidget.Status(Report(new AiUsageWindow("AiUsageWeekly", 1, null, 604800))) == ("AiUsageLimitReached", "WidgetFocusAccent"),
            "Status must follow thresholds or severity of session and weekly windows, with a reached limit first.");
        var session = new AiUsageWindow("AiUsageSession", 0.8, now.AddHours(2.5), 18000);
        var week = new AiUsageWindow("AiUsageWeekly", 0.9, now.AddDays(3.5), 604800);
        Check(AiUsageWidget.PaceWindow(Report(week, session), now) == session
            && AiUsageWidget.PaceWindow(Report(Slot(0.3, 2.5)), now) is null && AiUsageWidget.PaceWindow(Report(), now) is null,
            "Only the window that runs out soonest may carry a pace warning.");

        foreach (var json in new[] { "", "null", "{", "[]", "{}", "{\"five_hour\":true}", "{\"rate_limit\":null}" })
        {
            Check(AiUsageService.ParseClaude(json, null).Error == "ErrorAiUsageLoad", "Invalid Claude responses must return a localization key.");
            Check(AiUsageService.ParseCodex(json).Error == "ErrorAiUsageLoad", "Invalid Codex responses must return a localization key.");
            Check(AiUsageService.ParseAntigravity(json).Error == "ErrorAiUsageLoad", "Invalid Antigravity responses must return a localization key.");
            Check(AiUsageService.ParseOpenCode(json).Error == "ErrorAiUsageLoad", "Invalid OpenCode responses must return a localization key.");
        }

        var openCode = AiUsageService.ParseOpenCode("""
            {"rolling5h":{"usageDollars":2.34,"limitDollars":12,"usagePercent":19.5,"resetInSec":7200},
             "weekly":{"usageDollars":8.91,"limitDollars":30,"usagePercent":29.7,"resetInSec":345600},
             "monthly":{"usageDollars":15,"limitDollars":60,"resetInSec":1414800}}
            """).Report!;
        Check(openCode.Provider == AiProviders.OpenCode && openCode.Plan == "Go" && !openCode.LimitReached && openCode.Windows.Count == 3
            && openCode.Windows[0] is { LabelKey: "AiUsageSession", WindowSeconds: 18000 } && Math.Abs(openCode.Windows[0].Used - 0.195) < 1e-6
            && openCode.Windows[1] is { LabelKey: "AiUsageWeekly", WindowSeconds: 604800 } && Math.Abs(openCode.Windows[1].Used - 0.297) < 1e-6
            && openCode.Windows[2] is { LabelKey: "AiUsageMonthly", WindowSeconds: 2592000 } && Math.Abs(openCode.Windows[2].Used - 0.25) < 1e-6
            && openCode.Windows.All(window => window.ResetsAt is not null),
            "OpenCode Go must parse 5-hour, weekly and monthly rolling quota windows and derive percentage from dollars when usagePercent is omitted.");

        var agy = AiUsageService.ParseAntigravity("""
            {"buckets":[
                {"modelId":"chat_20706","remainingFraction":1},
                {"modelId":"gemini-2.5-pro","remainingFraction":0.6,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"gemini-4-hydrogen-sonar","remainingFraction":0.55,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"gemini-3-flash","remainingFraction":0.8,"resetTime":"2026-11-18T20:30:00Z"},
                {"modelId":"claude-sonnet-5-5-medium","remainingFraction":0.05,"resetTime":"2026-11-18T21:20:00Z"},
                {"modelId":"gpt-oss-120b-medium","remainingFraction":0.05,"resetTime":"2026-11-18T21:20:00Z"}
            ]}
            """, "Google AI Pro").Report!;
        Check(agy.Provider == AiProviders.Antigravity && agy.Plan == "Google AI Pro" && agy.Windows.Count == 3
            && agy.Windows[0] is { LabelKey: "AiUsagePool:Gemini Pro" } && Math.Abs(agy.Windows[0].Used - 0.45) < 1e-6
            && agy.Windows[1] is { LabelKey: "AiUsagePool:Gemini Flash" } && Math.Abs(agy.Windows[1].Used - 0.2) < 1e-6
            && agy.Windows[2] is { LabelKey: "AiUsagePool:Claude" } && Math.Abs(agy.Windows[2].Used - 0.95) < 1e-6,
            "Antigravity must group model buckets into Gemini Pro, Gemini Flash and Claude pools and deduplicate shared GPT-OSS quota.");
        var agyExhausted = AiUsageService.ParseAntigravity("""
            {"buckets":[{"modelId":"claude-opus-5-5-high","resetTime":"2026-11-18T21:20:00Z"}]}
            """).Report!;
        Check(agyExhausted.LimitReached && agyExhausted.Windows is [{ LabelKey: "AiUsagePool:Claude", Used: 1.0 }],
            "Antigravity buckets that omit remainingFraction when resetTime is set must register as 100% used.");
        var agyGrouped = AiUsageService.ParseAntigravity("""
            {"command":{"name":"usage","data":{"groups":[
                {"name":"Gemini Models","description":"Models within this group: Gemini 4 Hydrogen, Gemini 3.8 Flash",
                 "buckets":[{"id":"gemini-weekly","window":"weekly","remaining_fraction":0.5878,"reset_time":"2026-11-24T23:28:59Z"},
                            {"id":"gemini-5h","window":"5h","remaining_fraction":0.4554,"reset_time":"2026-11-18T20:26:15Z"}]},
                {"name":"Claude and GPT models","description":"Models within this group: Claude Opus 5.5, Claude Sonnet 5.5, GPT-OSS 120B",
                 "buckets":[{"id":"3p-weekly","window":"weekly","remaining_fraction":0.4907,"reset_time":"2026-11-25T16:21:48Z"},
                            {"id":"3p-5h","window":"5h","remaining_fraction":0.0,"reset_time":"2026-11-18T21:21:48Z"}]},
                {"name":"Gemini 4 Argon","description":"For paid Pro plans, Argon quota is separate.",
                 "buckets":[{"id":"gemini-argon-wtu-weekly","window":"weekly","remaining_fraction":0.6967,"reset_time":"2026-11-25T16:56:21Z"}]}
            ]}}}
            """).Report!;
        Check(!agyGrouped.LimitReached && agyGrouped.Windows.Count == 5
            && agyGrouped.Windows[0] is { Group: "AiUsageGroupGemini", GroupModels: "Gemini 4 Hydrogen, Gemini 3.8 Flash", LabelKey: "AiUsageWeekly", WindowSeconds: 604800 }
            && agyGrouped.Windows[1] is { Group: "AiUsageGroupGemini", LabelKey: "AiUsageSession", WindowSeconds: 18000 }
            && agyGrouped.Windows[3] is { Group: "AiUsageGroupClaudeGpt", LabelKey: "AiUsageSession", Used: 1.0 }
            && agyGrouped.Windows[4] is { Group: "Gemini 4 Argon", GroupModels: null, LabelKey: "AiUsageWeekly" }
            && AiUsageWidget.RingWindow(agyGrouped, AiUsageRing.Session)!.Used == 1.0
            && Math.Abs(AiUsageWidget.RingWindow(agyGrouped, AiUsageRing.Weekly)!.Used - (1 - 0.4907)) < 1e-6,
            "Antigravity grouped usage must preserve weekly and 5-hour windows per model group.");

        var path = Path.Combine(Path.GetTempPath(), $"ot-ai-fixture-{Guid.NewGuid():N}.json");
        try
        {
            Check(AiUsageService.ReadClaudeToken(path) is null && AiUsageService.ReadCodexToken(path) is null,
                "Missing fixture credentials must require CLI sign-in.");
            foreach (var expiry in new[] { 1L, DateTimeOffset.UtcNow.AddHours(1).ToUnixTimeMilliseconds() })
            {
                File.WriteAllText(path, JsonSerializer.Serialize(new { claudeAiOauth = new { accessToken = "fixture-only", expiresAt = expiry } }));
                var hash = SHA256.HashData(File.ReadAllBytes(path));
                Check(AiUsageService.ReadClaudeToken(path) == (expiry == 1 ? null : "fixture-only"),
                    "Expired Claude credentials must not be used.");
                Check(hash.SequenceEqual(SHA256.HashData(File.ReadAllBytes(path))), "Credential reads must leave fixture bytes unchanged.");
            }
            File.WriteAllText(path, """{"claudeAiOauth":{"accessToken":"","refreshToken":"fixture-refresh-desktop","scopes":["user:profile"],"subscriptionType":"pro","rateLimitTier":"default_claude_ai"}}""");
            Check(AiUsageService.ReadClaudeToken(path) is null, "A desktop credential without an accessToken must trigger refresh on load.");
            var refreshedToken = AiUsageService.TryRefreshClaudeTokenAsync(path, (refresh, _) =>
                Task.FromResult<string?>(refresh == "fixture-refresh-desktop"
                    ? """{"access_token":"fixture-refreshed-access","refresh_token":"fixture-rotated-refresh","expires_in":28800}"""
                    : null)).GetAwaiter().GetResult();
            Check(refreshedToken == "fixture-refreshed-access" && AiUsageService.ReadClaudeToken(path) == "fixture-refreshed-access",
                "Claude refresh must exchange refreshToken and persist the new accessToken and expiresAt back to .credentials.json.");
            using (var refreshedDoc = JsonDocument.Parse(File.ReadAllText(path)))
            {
                var oauth = refreshedDoc.RootElement.GetProperty("claudeAiOauth");
                Check(oauth.GetProperty("refreshToken").GetString() == "fixture-rotated-refresh"
                    && oauth.GetProperty("subscriptionType").GetString() == "pro"
                    && oauth.GetProperty("scopes").GetArrayLength() == 1,
                    "Claude refresh must persist the rotated refreshToken and preserve existing OAuth metadata.");
            }
            File.WriteAllText(path, """{"tokens":{"access_token":"fixture-codex","account_id":"fixture-account"}}""");
            Check(AiUsageService.ReadCodexToken(path) == "fixture-codex", "Codex must read its nested CLI token.");
            File.WriteAllText(path, """{"token":{"access_token":"fixture-agy"}}""");
            Check(AiUsageService.ReadAntigravityToken(path) == "fixture-agy", "Antigravity must read its nested keyring token.");
            File.WriteAllText(path, """{"access_token":"fixture-agy-flat","expiry_date":1}""");
            Check(AiUsageService.ReadAntigravityToken(path) is null, "Expired Antigravity oauth_creds must not be used.");
            File.WriteAllText(path, """{"opencode-go":{"type":"api","key":"fixture-oc-v1"}}""");
            Check(AiUsageService.ReadOpenCodeToken(path) == "fixture-oc-v1", "OpenCode must read v1 auth.json provider keys.");
            File.WriteAllText(path, """[{"integrationID":"ollama-cloud","active":true,"value":{"key":"other"}},{"integrationID":"opencode-go","active":true,"value":{"type":"key","key":"fixture-oc-v2"}}]""");
            Check(AiUsageService.ReadOpenCodeToken(path) == "fixture-oc-v2", "OpenCode must read v2 auth export array entries.");
            File.WriteAllText(path, """{"github.com:fixture":{"oauth_token":"fixture-copilot"}}""");
            Check(AiUsageService.ReadCopilotToken(path) == "fixture-copilot", "Copilot must read its oauth_token from apps.json.");
            var (copilotReport, copilotError) = AiUsageService.ParseCopilot("""
                {"copilot_plan":"individual","quota_reset_date_utc":"2026-12-01T00:00:00Z","quota_snapshots":{"premium_interactions":{"percent_remaining":72.5}}}
                """);
            Check(copilotError is null && copilotReport is { Provider: AiProviders.Copilot, Plan: "Individual", LimitReached: false }
                  && copilotReport.Windows is [{ LabelKey: "AiUsageMonthly", Used: >= 0.274 and <= 0.276, WindowSeconds: 2592000L }],
                "Copilot must parse plan, reset date, and premium interactions remaining percentage.");
            File.WriteAllText(path, """{"api_key":"fixture-deepseek"}""");
            Check(AiUsageService.ReadDeepSeekToken(path) == "fixture-deepseek", "DeepSeek must read its api_key from auth.json.");
            var (dsReport, dsError) = AiUsageService.ParseDeepSeek("""
                {"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"4.81"}]}
                """);
            Check(dsError is null && dsReport is { Provider: AiProviders.DeepSeek, Plan: "4.81 USD", Credits: 4.81, LimitReached: false }
                  && dsReport.Windows is [{ LabelKey: "AiUsageBalance", Used: 0.0 }],
                "DeepSeek must parse balance_infos and remaining balance.");
            File.WriteAllText(path, """{"api_key":"fixture-openrouter"}""");
            Check(AiUsageService.ReadOpenRouterToken(path) == "fixture-openrouter", "OpenRouter must read its api_key from auth.json.");
            var (orReport, orError) = AiUsageService.ParseOpenRouter("""
                {"data":{"total_credits":20.0,"total_usage":5.0}}
                """);
            Check(orError is null && orReport is { Provider: AiProviders.OpenRouter, Plan: "$15", Credits: 15.0, LimitReached: false }
                  && orReport.Windows is [{ LabelKey: "AiUsageCreditsLabel", Used: >= 0.249 and <= 0.251 }],
                "OpenRouter must parse total_credits and total_usage.");
            File.WriteAllText(path, "null");
            Check(AiUsageService.ReadClaudeToken(path) is null && AiUsageService.ReadCodexToken(path) is null,
                "Malformed credential files must fail closed.");
            var settings = new DockSettings(AiUsageProviders: AiProviders.All);
            var store = new DockSettingsStore(path);
            Check(!DockSettingsStore.Validate(settings with { AiUsageProviders = (AiProviders)128 }), "Unknown AI provider flags must be rejected.");
            Check(store.Save(settings) is null && store.Load().Settings.AiUsageProviders == AiProviders.All,
                "Provider flags must round-trip through settings.");
            File.WriteAllText(path, "{}");
            Check(store.Load().Settings is { AiUsageProviders: AiProviders.None, AiUsageRing: AiUsageRing.Highest },
                "Legacy settings must leave providers disabled and the ring on the highest window.");
            Check(!DockSettingsStore.Validate(settings with { AiUsageRing = (AiUsageRing)7 }), "Unknown ring windows must be rejected.");
            Check(store.Save(settings with { AiUsageRing = AiUsageRing.Weekly }) is null && store.Load().Settings.AiUsageRing == AiUsageRing.Weekly
                && File.ReadAllText(path).Contains("\"AiUsageRing\": \"Weekly\"", StringComparison.Ordinal),
                "The ring window must round-trip through settings by name.");
            Check(SettingsBackup.Export(path, settings) is null, "AI preferences must support settings export.");
            using var backup = JsonDocument.Parse(File.ReadAllText(path));
            var exported = backup.RootElement.GetProperty("Settings");
            Check(!exported.EnumerateObject().Any(property => property.Name.Contains("token", StringComparison.OrdinalIgnoreCase)
                || property.Name.Contains("credential", StringComparison.OrdinalIgnoreCase)), "Settings export must contain no credential fields.");
        }
        finally { File.Delete(path); }

        Check(AiUsageService.HttpFailure(HttpStatusCode.Unauthorized, null).Error == "ErrorAiUsageSignIn",
            "Only unauthorized responses must require CLI sign-in.");
        foreach (var status in new[] { HttpStatusCode.Forbidden, HttpStatusCode.Gone, HttpStatusCode.NotFound })
        {
            var failure = AiUsageService.HttpFailure(status, null);
            Check(failure.Error == "ErrorAiUsageLoad" && failure.RetryAfter is null
                && AiUsageWidget.RetryDelay(failure.Error, 1, failure.RetryAfter) == TimeSpan.FromMinutes(2),
                "Forbidden and removed endpoints must use normal error backoff.");
        }
        var retry = AiUsageService.HttpFailure(HttpStatusCode.TooManyRequests, new RetryConditionHeaderValue(TimeSpan.FromMinutes(42)));
        Check(retry.Error == "ErrorAiUsageRateLimited" && AiUsageWidget.RetryDelay(retry.Error, 1, retry.RetryAfter) == TimeSpan.FromMinutes(42),
            "Retry-After must override exponential backoff, even beyond the default cap.");
        Check(AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 1, null) == TimeSpan.FromMinutes(5)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 2, null) == TimeSpan.FromMinutes(10)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 3, null) == TimeSpan.FromMinutes(20)
            && AiUsageWidget.RetryDelay("ErrorAiUsageRateLimited", 4, null) == TimeSpan.FromMinutes(30), "Rate limiting must back off to a thirty-minute cap.");
        Check(AiUsageWidget.RetryDelay("ErrorAiUsageLoad", 1, null) == TimeSpan.FromMinutes(2)
            && AiUsageWidget.RetryDelay("ErrorAiUsageLoad", 2, null) == TimeSpan.FromMinutes(4), "Network failures must use exponential retries.");
        Check(AiUsageWidget.RetryDelay(null, 0, null).TotalSeconds is >= 300 and <= 330, "Successful loads must refresh after five minutes plus jitter.");

        string[] keys = ["WidgetAiUsage", "WidgetAiUsageDescription", "AiUsageSettingsHeading", "AiUsageProviderHint",
            "AiUsageUnofficialNote", "AiUsageSession", "AiUsageWeekly", "AiUsageMonthly", "AiUsageWeeklyModel", "AiUsageGroupGemini", "AiUsageGroupClaudeGpt", "AiUsageResetsIn", "AiUsageResetsAt",
            "AiUsageUpdated", "AiUsageRefresh", "AiUsageOpenDashboard", "AiUsageNoProviders", "AiUsageRingLabel",
            "AiUsageRingHighest", "AiUsageRingSession", "AiUsageLimitReached", "AiUsageStatusOk", "AiUsageStatusSignIn",
            "AiUsageStatusStale", "AiUsageStatusNearLimit", "AiUsageStatusHigh", "AiUsageStatusUnavailable", "AiUsageStatusLoading", "AiUsagePaceIn", "AiUsagePaceAt", "AiUsagePaceMarker", "HoursMinutesFormat",
            "AiUsageExtra", "AiUsageBreakdown", "AiUsageCredits", "AiUsageCreditsUnlimited", "AiUsageResetCredits",
            "ErrorAiUsageSignIn", "ErrorAiUsageLoad", "ErrorAiUsageRateLimited"];
        var assembly = typeof(LanguageCatalog).Assembly;
        foreach (var resource in assembly.GetManifestResourceNames().Where(name => name.StartsWith("OpenTaskbar.Localization.Languages.", StringComparison.Ordinal)))
        {
            using var stream = assembly.GetManifestResourceStream(resource)!;
            using var document = JsonDocument.Parse(stream);
            Check(keys.All(key => document.RootElement.TryGetProperty(key, out var value) && !string.IsNullOrWhiteSpace(value.GetString())),
                $"Every AI usage key must be translated in {resource}.");
        }

        // Widgets need an STA thread; this also runs headless, without application resources.
        Exception? renderFailure = null;
        var thread = new Thread(() =>
        {
            try { checks += EmptyRenderChecks(); }
            catch (Exception ex) { renderFailure = ex; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (renderFailure is not null) System.Runtime.ExceptionServices.ExceptionDispatchInfo.Capture(renderFailure).Throw();
        return checks;
    }

    private static int EmptyRenderChecks()
    {
        var checks = 0;
        var pending = new TaskCompletionSource<(AiUsageReport?, string?)>();
        Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>[] loaders =
        [
            (_, _) => pending.Task,
            (provider, _) => Task.FromResult<(AiUsageReport?, string?)>((new(provider, null, [], DateTimeOffset.UtcNow), null))
        ];
        foreach (var loader in loaders)
            foreach (var providers in new[] { AiProviders.None, AiProviders.Claude, AiProviders.All })
                foreach (var compact in new[] { false, true })
                {
                    using var widget = new AiUsageWidget(loader);
                    widget.ApplySettings(new DockSettings(AiUsageProviders: providers));
                    widget.Activate();
                    widget.SetCompactState(compact);
                    widget.Tick(DateTime.Now.AddMinutes(1));
                    if (widget.BuildHoverFlyoutContent() is null)
                        throw new InvalidOperationException("AI Usage must build a flyout without any loaded report.");
                    widget.SetCompactState(!compact);
                    checks++;
                }
        pending.SetCanceled();
        return checks;
    }
}
