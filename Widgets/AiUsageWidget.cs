using System.Diagnostics;
using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>Opt-in CLI quota meters; only active, enabled providers may poll.</summary>
public sealed class AiUsageWidget : DockWidget
{
    private static readonly AiProviders[] Providers = [AiProviders.Claude, AiProviders.Codex, AiProviders.Antigravity, AiProviders.OpenCode, AiProviders.Copilot, AiProviders.DeepSeek, AiProviders.OpenRouter];
    private readonly Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>? _loader;
    private readonly Dictionary<AiProviders, AiUsageReport> _reports = [];
    private readonly Dictionary<AiProviders, string> _errors = [];
    private readonly Dictionary<AiProviders, DateTimeOffset> _nextLoadAt = [];
    private readonly Dictionary<AiProviders, DateTime> _signInStamps = [];
    private readonly Dictionary<AiProviders, int> _failures = [];
    private readonly Dictionary<AiProviders, DateTimeOffset> _lastLoadAt = [];
    // Each built flyout keeps its own countdowns: the click flyout and the compact hover flyout can coexist.
    private readonly List<(TextBlock Block, DateTimeOffset Reset)> _countdowns = [];
    private readonly List<(TextBlock Block, DateTimeOffset Reset)> _hoverCountdowns = [];
    private readonly ContentControl _flyoutHost = new();
    private readonly StackPanel _cards = new();
    private readonly Card _summary;
    private readonly Card _claude;
    private readonly Card _codex;
    private readonly Card _antigravity;
    private readonly Card _openCode;
    private readonly Card _copilot;
    private readonly Card _deepSeek;
    private readonly Card _openRouter;
    private readonly DockFlyout _flyout = new();
    private CancellationTokenSource? _loading;
    private AiProviders _loadingProvider;
    private AiProviders _enabled;
    private int _renderedMinute = -1;

    private sealed record Card(Button Button, System.Windows.Shapes.Path Logo, TextBlock Name, RingMeter Ring);

    public AiUsageWidget(Func<AiProviders, CancellationToken, Task<(AiUsageReport?, string?)>>? loader = null)
    {
        _loader = loader;
        _summary = AddCard("AI", null, AiProviders.None);
        _claude = AddCard("Claude", "LogoClaude", AiProviders.Claude);
        _codex = AddCard("Codex", "LogoCodex", AiProviders.Codex);
        _antigravity = AddCard("Antigravity", "LogoAntigravity", AiProviders.Antigravity);
        _openCode = AddCard("OpenCode", "LogoOpenCode", AiProviders.OpenCode);
        _copilot = AddCard("Copilot", "LogoCopilot", AiProviders.Copilot);
        _deepSeek = AddCard("DeepSeek", "LogoDeepSeek", AiProviders.DeepSeek);
        _openRouter = AddCard("OpenRouter", "LogoOpenRouter", AiProviders.OpenRouter);
        _flyout.Closed += (_, _) => _countdowns.Clear();
        Content = _cards;
        Render();
    }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 40;
    // Card layout: padding and border, logo, name, ring; the compact summary card has no logo.
    private const double LogoSize = 16, NameWidth = 44, RingSize = 34, Gap = 6;
    private const double CardPaddingAndBorder = 22;
    public override double FullWidthEstimate => Math.Max(1, EnabledProviders.Count()) * (CardWidth(true) + CardSpacing);
    public override double CompactWidthEstimate => CardWidth(false) + CardSpacing;
    private static double CardWidth(bool logo) => CardPaddingAndBorder + (logo ? LogoSize + Gap : 0) + NameWidth + Gap + RingSize;
    private double CardSpacing => TryFindResource("WidgetSpacing") is Thickness spacing ? spacing.Left + spacing.Right : 0;
    private IEnumerable<AiProviders> EnabledProviders => Providers.Where(provider => _enabled.HasFlag(provider));

    protected override void OnActivated()
    {
        _enabled = Settings.AiUsageProviders;
        Render();
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        Clear();
        _flyout.Close();
    }

    private void Clear()
    {
        _loading?.Cancel();
        _loading = null;
        _reports.Clear();
        _errors.Clear();
        _nextLoadAt.Clear();
        _signInStamps.Clear();
        _failures.Clear();
        _lastLoadAt.Clear();
    }

    protected override void OnSettingsChanged()
    {
        if (_enabled != Settings.AiUsageProviders)
        {
            foreach (var provider in Providers.Where(provider => _enabled.HasFlag(provider) != Settings.AiUsageProviders.HasFlag(provider)))
            {
                _reports.Remove(provider);
                _errors.Remove(provider);
                _nextLoadAt.Remove(provider);
                _signInStamps.Remove(provider);
                _failures.Remove(provider);
                _lastLoadAt.Remove(provider);
                if (_loadingProvider == provider)
                {
                    _loading?.Cancel();
                    _loading = null;
                }
            }
            _flyout.Close();
            _enabled = Settings.AiUsageProviders;
            if (IsActive) _ = LoadAsync();
        }
        Render();
    }

    protected override void OnLanguageChanged() => Refresh();
    protected override void OnCompactChanged(bool compact) => Render();
    public override UIElement? BuildHoverFlyoutContent() => BuildFlyout(_hoverCountdowns);
    public override UIElement? BuildFullHoverFlyoutContent() => BuildHoverFlyoutContent();

    protected override void OnTick(DateTime now)
    {
        if (_loading is null) _ = LoadAsync();
        // Reset times and staleness only change by the minute; open flyouts keep counting down.
        if (now.Minute != _renderedMinute) Render();
        if (!IsHoverFlyoutOpen) _hoverCountdowns.Clear();
        foreach (var (block, reset) in _countdowns.Concat(_hoverCountdowns)) block.Text = ResetText(reset);
    }

    /// <summary>Re-renders the cards and, when our click flyout is open, swaps its content in place without reopening it.</summary>
    private void Refresh()
    {
        Render();
        if (_flyout.IsOpen) _flyoutHost.Content = BuildFlyout(_countdowns);
    }

    private static bool IsStale(AiUsageReport report) => DateTimeOffset.UtcNow - report.FetchedAt > TimeSpan.FromMinutes(10);

    private bool IsDue(AiProviders provider, DateTime credentialStamp)
    {
        if (_signInStamps.TryGetValue(provider, out var stamp))
        {
            if (_loader is not null || credentialStamp == stamp) return false;
            _signInStamps.Remove(provider);
            _nextLoadAt.Remove(provider);
        }
        return !_nextLoadAt.TryGetValue(provider, out var next) || DateTimeOffset.UtcNow >= next;
    }

    private async Task LoadAsync()
    {
        if (!IsActive || _loading is not null) return;
        var due = EnabledProviders.Where(provider => _signInStamps.ContainsKey(provider)
            || !_nextLoadAt.TryGetValue(provider, out var next) || DateTimeOffset.UtcNow >= next).ToArray();
        if (due.Length == 0) return;
        var loading = new CancellationTokenSource();
        _loading = loading;
        try
        {
            foreach (var provider in due)
            {
                _loadingProvider = provider;
                var stamp = _loader is null ? await Task.Run(() => AiUsageService.CredentialStamp(provider), loading.Token) : DateTime.MinValue;
                if (!ReferenceEquals(_loading, loading) || !IsActive || loading.IsCancellationRequested) return;
                if (!_enabled.HasFlag(provider) || !IsDue(provider, stamp)) continue;
                _lastLoadAt[provider] = DateTimeOffset.UtcNow;
                AiUsageService.LoadResult result;
                if (_loader is null) result = await AiUsageService.LoadWithPolicyAsync(provider, loading.Token);
                else
                {
                    var (report, error) = await _loader(provider, loading.Token);
                    result = new(report, error);
                }
                if (!ReferenceEquals(_loading, loading) || !IsActive || loading.IsCancellationRequested) return;
                if (!_enabled.HasFlag(provider)) continue;
                if (result.Report is { } loaded) _reports[provider] = loaded;
                if (result.Error is { } failure) _errors[provider] = failure;
                else _errors.Remove(provider);
                if (result.Error == "ErrorAiUsageSignIn") _signInStamps[provider] = stamp;
                var failures = result.Error is null ? 0 : _failures.GetValueOrDefault(provider) + 1;
                _failures[provider] = Math.Min(failures, 10);
                var delay = RetryDelay(result.Error, failures, result.RetryAfter);
                _nextLoadAt[provider] = DateTimeOffset.UtcNow + delay;
                Refresh();
            }
        }
        catch (OperationCanceledException) when (loading.IsCancellationRequested) { }
        catch (Exception ex)
        {
            if (!ReferenceEquals(_loading, loading) || !IsActive) return;
            _errors[_loadingProvider] = "ErrorAiUsageLoad";
            var failures = Math.Min(10, _failures.GetValueOrDefault(_loadingProvider) + 1);
            _failures[_loadingProvider] = failures;
            _nextLoadAt[_loadingProvider] = DateTimeOffset.UtcNow + RetryDelay("ErrorAiUsageLoad", failures, null);
            AppLog.Info($"AI usage unavailable: {ex.GetType().Name}");
        }
        finally
        {
            if (ReferenceEquals(_loading, loading)) _loading = null;
            loading.Dispose();
        }
    }

    internal static TimeSpan RetryDelay(string? error, int failures, TimeSpan? retryAfter)
    {
        if (retryAfter is { } specified) return specified > TimeSpan.Zero ? specified : TimeSpan.FromSeconds(1);
        if (error is null) return TimeSpan.FromSeconds(300 + Random.Shared.Next(31));
        var minutes = error == "ErrorAiUsageRateLimited" ? 5 : 2;
        return TimeSpan.FromMinutes(Math.Min(30, minutes * Math.Pow(2, Math.Clamp(failures - 1, 0, 10))));
    }

    private static string ProviderName(AiProviders provider) => provider switch
    {
        AiProviders.Claude => "Claude Code",
        AiProviders.Codex => "Codex",
        AiProviders.OpenCode => "OpenCode Go",
        AiProviders.Copilot => "GitHub Copilot",
        AiProviders.DeepSeek => "DeepSeek",
        AiProviders.OpenRouter => "OpenRouter",
        _ => "Antigravity"
    };
    private string ErrorText(AiProviders provider, string key) => key == "ErrorAiUsageSignIn"
        ? UiText.Format(key, provider switch
        {
            AiProviders.Claude => "claude auth login",
            AiProviders.Codex => "codex login",
            AiProviders.OpenCode => "opencode auth login",
            AiProviders.Copilot => "gh auth login",
            AiProviders.DeepSeek => "DEEPSEEK_API_KEY",
            AiProviders.OpenRouter => "OPENROUTER_API_KEY",
            _ => "agy"
        }) : UiText.Get(key);
    private CultureInfo Culture => Settings.Language.Culture;
    private static bool IsModel(AiUsageWindow window) => window.LabelKey.StartsWith("AiUsageWeeklyModel:", StringComparison.Ordinal);
    private static bool IsPool(AiUsageWindow window) => window.LabelKey.StartsWith("AiUsagePool:", StringComparison.Ordinal);
    private static bool IsLimitReached(AiUsageReport report) => report.LimitReached
        || (report.Provider != AiProviders.Antigravity && report.Windows.Any(window => window.Used >= 1 && !IsModel(window)));
    private static string Percent(double value) => UiText.Format("Percent",
        ((int)Math.Round(value * 100)).ToString(CultureInfo.InvariantCulture));

    /// <summary>Accent for a usage level; a provider severity can raise it but never hide a high reading.</summary>
    internal static string Accent(double value, string? severity = null)
    {
        var level = value >= 0.9 ? 2 : value >= 0.75 ? 1 : 0;
        if (severity is not null && !severity.Equals("normal", StringComparison.OrdinalIgnoreCase))
            level = Math.Max(level, severity.Contains("warn", StringComparison.OrdinalIgnoreCase) ? 1
                : severity.Contains("crit", StringComparison.OrdinalIgnoreCase)
                    || severity.Contains("exceed", StringComparison.OrdinalIgnoreCase)
                    || severity.Contains("block", StringComparison.OrdinalIgnoreCase) ? 2 : 0);
        return level == 2 ? "WidgetFocusAccent" : level == 1 ? "WidgetWarning" : "WidgetAccent";
    }

    /// <summary>The window the dock ring shows; a missing session or weekly window falls back to the highest.</summary>
    internal static AiUsageWindow? RingWindow(AiUsageReport report, AiUsageRing ring) => ring switch
    {
        AiUsageRing.Session => report.Windows.Where(window => window.LabelKey == "AiUsageSession").MaxBy(window => window.Used),
        AiUsageRing.Weekly => report.Windows.Where(window => window.LabelKey == "AiUsageWeekly").MaxBy(window => window.Used),
        _ => null
    } ?? Highest(report);

    /// <summary>The fullest session or weekly window; per-model windows only count when nothing else is reported.</summary>
    private static AiUsageWindow? Highest(AiUsageReport report) =>
        report.Windows.Where(window => !IsModel(window)).MaxBy(window => window.Used) ?? report.Windows.MaxBy(window => window.Used);

    /// <summary>Status for a loaded report: limit reached, then the provider severity or the 75%/90% thresholds.</summary>
    internal static (string Key, string Brush) Status(AiUsageReport report)
    {
        if (IsLimitReached(report)) return ("AiUsageLimitReached", "WidgetFocusAccent");
        var level = report.Windows.Where(window => !IsModel(window)).Select(window => Accent(window.Used, window.Severity)).ToArray();
        return level.Contains("WidgetFocusAccent") ? ("AiUsageStatusNearLimit", "WidgetFocusAccent")
            : level.Contains("WidgetWarning") ? ("AiUsageStatusHigh", "WidgetWarning") : ("AiUsageStatusOk", "WidgetAccent");
    }

    /// <summary>Only the window that would run out soonest carries a pace warning, so one provider shows at most one.</summary>
    internal static AiUsageWindow? PaceWindow(AiUsageReport report, DateTimeOffset now) => report.Windows
        .Select(window => (Window: window, Limit: LimitIn(window, now))).Where(item => item.Limit is not null)
        .OrderBy(item => item.Limit).Select(item => item.Window).FirstOrDefault();

    /// <summary>Share of the window that has elapsed, or null when its length or reset time is unknown.</summary>
    internal static double? Elapsed(AiUsageWindow window, DateTimeOffset now)
    {
        if (window.WindowSeconds is not { } length || length <= 0 || window.ResetsAt is not { } reset) return null;
        var remaining = (reset - now).TotalSeconds;
        return remaining <= 0 || remaining > length ? null : 1 - remaining / length;
    }

    /// <summary>Time until the limit at the window's average rate so far, only when that comes before the reset.</summary>
    internal static TimeSpan? LimitIn(AiUsageWindow window, DateTimeOffset now)
    {
        // The first minutes of a window say little about the rate (5% is 15 minutes of a five-hour session).
        if (Elapsed(window, now) is not { } elapsed || elapsed < 0.05 || window.Used is <= 0 or >= 1
            || window.WindowSeconds is not { } seconds) return null;
        var length = (double)seconds;
        var toLimit = (1 - window.Used) * elapsed * length / window.Used;
        return toLimit < (1 - elapsed) * length ? TimeSpan.FromSeconds(Math.Max(60, toLimit)) : null;
    }

    private string Label(AiUsageWindow window)
    {
        if (IsPool(window)) return window.LabelKey[12..];
        if (!IsModel(window)) return UiText.Get(window.LabelKey);
        var model = window.LabelKey.Split(':', 2)[1].Replace('_', ' ');
        return UiText.Format("AiUsageWeeklyModel", model.Length == 0 ? model : char.ToUpper(model[0], Culture) + model[1..]);
    }

    private static string GroupLabel(string group) =>
        group.StartsWith("AiUsageGroup", StringComparison.Ordinal) ? UiText.Get(group) : group;

    private string SummaryWindowLabel(AiUsageWindow window) =>
        window.Group is { Length: > 0 } group ? $"{GroupLabel(group)} ({Label(window)})" : Label(window);

    // "h:mm" reads as a clock time once it nears a day, so longer spans show the local weekday and time instead.
    private static readonly TimeSpan RelativeLimit = TimeSpan.FromHours(6);

    private string ResetText(DateTimeOffset reset)
    {
        var remaining = reset - DateTimeOffset.UtcNow;
        return remaining < RelativeLimit
            ? UiText.Format("AiUsageResetsIn", Duration(remaining))
            : UiText.Format("AiUsageResetsAt", When(reset));
    }

    private string When(DateTimeOffset moment)
    {
        var local = moment.ToLocalTime();
        return moment - DateTimeOffset.UtcNow < TimeSpan.FromDays(6)
            ? $"{local.ToString("ddd", Culture)} {local.ToString("t", Culture)}" : local.ToString("g", Culture);
    }

    private string Duration(TimeSpan span)
    {
        var value = span < TimeSpan.Zero ? TimeSpan.Zero : span;
        var minutes = Math.Max(1, (int)Math.Ceiling(value.TotalMinutes));
        // Spelled-out units: a bare "1:54" reads as a clock time, not a duration.
        return minutes < 60 ? UiText.Format("MinutesFormat", minutes) : UiText.Format("HoursMinutesFormat", minutes / 60, minutes % 60);
    }

    private string Summary(AiProviders provider)
    {
        var name = ProviderName(provider);
        _errors.TryGetValue(provider, out var error);
        if (!_reports.TryGetValue(provider, out var report) || RingWindow(report, Settings.AiUsageRing) is not { } main)
            return error is null ? name : $"{name}: {ErrorText(provider, error)}";
        var parts = new List<string> { $"{name} {Percent(main.Used)}" };
        parts.AddRange(report.Windows.Where(window => window != main && !IsModel(window))
            .Select(window => $"{SummaryWindowLabel(window)} {Percent(window.Used)}"));
        if (IsLimitReached(report)) parts.Add(UiText.Get("AiUsageLimitReached"));
        else if (main.ResetsAt is { } reset) parts.Add(ResetText(reset));
        if (error is not null) parts.Add(ErrorText(provider, error));
        return string.Join(" · ", parts);
    }

    private Card AddCard(string name, string? logoKey, AiProviders provider)
    {
        var logo = new System.Windows.Shapes.Path { Width = LogoSize, Height = LogoSize, Stretch = Stretch.Uniform,
            Margin = new Thickness(0, 0, Gap, 0), VerticalAlignment = VerticalAlignment.Center, IsHitTestVisible = false };
        if (logoKey is null) logo.Visibility = Visibility.Collapsed;
        else logo.Data = (Geometry)TryFindResource(logoKey);
        logo.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, "TextPrimary");
        var label = Text(name, 12, "TextPrimary");
        label.FontWeight = FontWeights.SemiBold;
        label.Margin = new Thickness(0, 0, Gap, 0);
        var ring = new RingMeter { Width = RingSize, Height = RingSize, StrokeThickness = 3, FontSize = 10.5, Text = "—", IsHitTestVisible = false };
        ring.SetResourceReference(RingMeter.TrackBrushProperty, "TextMuted");
        var row = new StackPanel { Orientation = Orientation.Horizontal, Children = { logo, label, ring } };
        var button = new Button { Content = row };
        button.SetResourceReference(StyleProperty, "WidgetCardButton");
        WidgetTint.Apply(button, WidgetTint.ForAiProvider(provider));
        button.Click += (_, _) =>
        {
            if (IsHoverFlyoutOpen) return;
            _flyout.Toggle(button, () =>
            {
                _flyoutHost.Content = BuildFlyout(_countdowns);
                return _flyoutHost;
            });
        };
        _cards.Children.Add(button);
        return new(button, logo, label, ring);
    }
    private void Render()
    {
        HasData = true;
        _renderedMinute = DateTime.Now.Minute;
        _cards.Orientation = Settings.Edge == DockEdge.Bottom ? Orientation.Horizontal : Orientation.Vertical;
        var providers = EnabledProviders.ToArray();
        var summary = IsCompact || providers.Length == 0;
        if (summary)
        {
            var reports = providers.Where(_reports.ContainsKey).Select(provider => _reports[provider]).ToArray();
            // MaxBy over reference types returns null for an empty sequence, so nothing loaded yet shows a dash.
            var highest = reports.Select(report => RingWindow(report, Settings.AiUsageRing)).OfType<AiUsageWindow>()
                .MaxBy(window => window.Used);
            UpdateCard(_summary, highest, reports.Any(IsStale),
                providers.Length == 0 ? UiText.Get("AiUsageNoProviders") : string.Join("\n", providers.Select(Summary)));
        }
        _summary.Button.Visibility = summary ? Visibility.Visible : Visibility.Collapsed;
        foreach (var (provider, card) in new[] { (AiProviders.Claude, _claude), (AiProviders.Codex, _codex), (AiProviders.Antigravity, _antigravity), (AiProviders.OpenCode, _openCode), (AiProviders.Copilot, _copilot), (AiProviders.DeepSeek, _deepSeek), (AiProviders.OpenRouter, _openRouter) })
        {
            var visible = !summary && _enabled.HasFlag(provider);
            card.Button.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
            if (!visible) continue;
            if (!_reports.TryGetValue(provider, out var report) || RingWindow(report, Settings.AiUsageRing) is not { } main)
            {
                UpdateCard(card, null, false, Summary(provider));
                continue;
            }
            UpdateCard(card, main, IsStale(report), Summary(provider));
        }
    }

    private void UpdateCard(Card card, AiUsageWindow? main, bool stale, string toolTip)
    {
        card.Ring.Value = main?.Used ?? 0;
        card.Ring.Text = main is null ? "—" : ((int)Math.Round(main.Used * 100)).ToString(CultureInfo.InvariantCulture);
        card.Ring.SetResourceReference(RingMeter.ValueBrushProperty, main is null ? "TextMuted" : Accent(main.Used, main.Severity));
        var foreground = stale ? "TextMuted" : "TextPrimary";
        card.Ring.SetResourceReference(RingMeter.ForegroundProperty, foreground);
        card.Name.SetResourceReference(TextBlock.ForegroundProperty, foreground);
        card.Logo.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, foreground);
        // The hover card shows the details; the one-line summary stays for screen readers.
        System.Windows.Automation.AutomationProperties.SetHelpText(card.Button, toolTip);
        AutomationProperties.SetName(card.Button, $"{UiText.Get("WidgetAiUsage")}: {toolTip}");
    }
    private UIElement BuildFlyout(List<(TextBlock Block, DateTimeOffset Reset)> countdowns)
    {
        countdowns.Clear();
        var now = DateTimeOffset.UtcNow;
        var root = new StackPanel { Width = 280 };
        if (_enabled == AiProviders.None)
        {
            root.Children.Add(DockFlyout.Heading(UiText.Get("WidgetAiUsage")));
            root.Children.Add(DockFlyout.Caption(UiText.Get("AiUsageNoProviders")));
            return root;
        }
        foreach (var provider in EnabledProviders)
        {
            if (root.Children.Count > 0) root.Children.Add(Divider());
            _reports.TryGetValue(provider, out var report);
            root.Children.Add(SectionHeader(provider, report));
            if (_errors.TryGetValue(provider, out var error))
            {
                var message = Text(ErrorText(provider, error), 11.5, "TextSecondary", new Thickness(0, 0, 0, 8));
                message.TextWrapping = TextWrapping.Wrap;
                root.Children.Add(message);
            }
            if (report is null) continue;
            var pace = PaceWindow(report, now);
            string? currentGroup = null;
            foreach (var window in report.Windows)
            {
                if (window.Group is { Length: > 0 } group && !string.Equals(group, currentGroup, StringComparison.Ordinal))
                {
                    var isFirstGroup = currentGroup is null;
                    currentGroup = group;
                    var groupHeader = Text(GroupLabel(group), 11.5, "TextPrimary",
                        new Thickness(0, isFirstGroup ? 0 : 4, 0, window.GroupModels is null ? 4 : 1));
                    groupHeader.FontWeight = FontWeights.SemiBold;
                    root.Children.Add(groupHeader);
                    if (window.GroupModels is { Length: > 0 } models)
                    {
                        var subtitle = Text(models, 10, "TextSecondary", new Thickness(0, 0, 0, 5));
                        subtitle.TextWrapping = TextWrapping.Wrap;
                        root.Children.Add(subtitle);
                    }
                }
                root.Children.Add(WindowRow(window, now, window == pace, countdowns));
            }
            if (report.Spend is { } spend) root.Children.Add(SpendRow(spend));
            if (report.Breakdown.Count > 0) root.Children.Add(BreakdownTiles(report.Breakdown));
            var extras = new List<string>();
            if (report.UnlimitedCredits) extras.Add(UiText.Get("AiUsageCreditsUnlimited"));
            else if (report.Credits is { } credits) extras.Add(UiText.Format("AiUsageCredits", credits.ToString("0.##", Culture)));
            if (report.ResetCredits > 0) extras.Add(UiText.Format("AiUsageResetCredits", report.ResetCredits.ToString(Culture)));
            if (extras.Count > 0) root.Children.Add(Text(string.Join(" · ", extras), 11, "TextSecondary", new Thickness(0, 0, 0, 6)));
        }
        root.Children.Add(Divider());
        root.Children.Add(Footer());
        return root;
    }

    private UIElement SectionHeader(AiProviders provider, AiUsageReport? report)
    {
        var (status, brush) = _errors.GetValueOrDefault(provider) == "ErrorAiUsageSignIn" ? ("AiUsageStatusSignIn", "WidgetWarning")
            : report is not null && IsLimitReached(report) ? Status(report)
            : report is not null && IsStale(report) ? ("AiUsageStatusStale", "TextMuted")
            : report is not null ? Status(report)
            : _errors.ContainsKey(provider) ? ("AiUsageStatusUnavailable", "TextMuted") : ("AiUsageStatusLoading", "TextMuted");
        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 8) };
        var open = IconButton("IconOpen", "AiUsageOpenDashboard", 12, () => OpenDashboard(provider));
        open.Width = open.Height = 22;
        open.Margin = new Thickness(6, -3, -3, -3);
        DockPanel.SetDock(open, Dock.Right);
        header.Children.Add(open);
        var state = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var dot = new Border { Width = 6, Height = 6, CornerRadius = new CornerRadius(3), Margin = new Thickness(0, 1, 5, 0) };
        dot.SetResourceReference(Border.BackgroundProperty, brush);
        state.Children.Add(dot);
        state.Children.Add(Text(UiText.Get(status), 11, "TextSecondary"));
        DockPanel.SetDock(state, Dock.Right);
        header.Children.Add(state);
        var title = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var name = Text(ProviderName(provider), 13, "TextPrimary");
        name.FontWeight = FontWeights.SemiBold;
        title.Children.Add(name);
        if (report?.Plan is { } plan)
        {
            var chip = new Border { CornerRadius = new CornerRadius(6), Padding = new Thickness(6, 0, 6, 1), Margin = new Thickness(6, 1, 0, 0),
                VerticalAlignment = VerticalAlignment.Center, Child = Text(plan, 10.5, "TextSecondary") };
            chip.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
            title.Children.Add(chip);
        }
        header.Children.Add(title);
        return header;
    }

    private UIElement WindowRow(AiUsageWindow window, DateTimeOffset now, bool showPace,
        List<(TextBlock Block, DateTimeOffset Reset)> countdowns)
    {
        var row = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
        row.Children.Add(ValueLine(Label(window), Percent(window.Used)));
        row.Children.Add(Bar(window.Used, Accent(window.Used, window.Severity), Elapsed(window, now)));
        if (window.ResetsAt is { } reset)
        {
            var resets = Text(ResetText(reset), 11, "TextSecondary");
            countdowns.Add((resets, reset));
            row.Children.Add(resets);
        }
        if (showPace && LimitIn(window, now) is { } limit)
            row.Children.Add(Text(limit < RelativeLimit ? UiText.Format("AiUsagePaceIn", Duration(limit))
                : UiText.Format("AiUsagePaceAt", When(now + limit)), 11, Accent(window.Used, window.Severity)));
        return row;
    }

    private UIElement SpendRow(AiUsageSpend spend)
    {
        var used = spend.Used01 ?? (spend.Limit is > 0m ? (double?)(double)(spend.Used / spend.Limit.Value) : null);
        var amount = spend.Used.ToString("N2", Culture) + (spend.Limit is { } limit ? " / " + limit.ToString("N2", Culture) : string.Empty)
            + " " + spend.Currency;
        var row = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
        row.Children.Add(ValueLine(UiText.Get("AiUsageExtra"), amount));
        if (used is { } value) row.Children.Add(Bar(Math.Clamp(value, 0, 1), Accent(value), null));
        return row;
    }

    private UIElement BreakdownTiles(IReadOnlyList<AiUsageShare> rows)
    {
        var panel = new StackPanel { Margin = new Thickness(0, 0, 0, 6) };
        var heading = Text(UiText.Get("AiUsageBreakdown"), 11, "TextSecondary", new Thickness(0, 0, 0, 4));
        heading.FontWeight = FontWeights.SemiBold;
        panel.Children.Add(heading);
        var grid = new UniformGrid { Columns = Math.Min(3, rows.Count), Margin = new Thickness(-2, 0, -2, 0) };
        foreach (var share in rows)
        {
            var tile = new Border { CornerRadius = new CornerRadius(8), Padding = new Thickness(8, 5, 8, 5), Margin = new Thickness(2) };
            tile.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
            var value = Text(Percent(share.Used), 12.5, "TextPrimary");
            value.FontWeight = FontWeights.SemiBold;
            Typography.SetNumeralAlignment(value, FontNumeralAlignment.Tabular);
            var label = Text(share.Name, 10.5, "TextSecondary");
            label.TextTrimming = TextTrimming.CharacterEllipsis;
            tile.Child = new StackPanel { Children = { label, value } };
            grid.Children.Add(tile);
        }
        panel.Children.Add(grid);
        return panel;
    }

    private UIElement Footer()
    {
        var footer = new DockPanel();
        var refresh = IconButton("IconRefresh", "AiUsageRefresh", 14, () =>
        {
            foreach (var provider in EnabledProviders.Where(provider => !_errors.ContainsKey(provider)
                && (!_lastLoadAt.TryGetValue(provider, out var last) || DateTimeOffset.UtcNow - last >= TimeSpan.FromSeconds(30))))
                _nextLoadAt.Remove(provider);
            _ = LoadAsync();
        });
        refresh.VerticalAlignment = VerticalAlignment.Top;
        DockPanel.SetDock(refresh, Dock.Right);
        footer.Children.Add(refresh);
        var notes = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var fetched = EnabledProviders.Where(_reports.ContainsKey).Select(provider => _reports[provider].FetchedAt).ToArray();
        if (fetched.Length > 0)
            notes.Children.Add(Text(UiText.Format("AiUsageUpdated", fetched.Min().ToLocalTime().ToString("t", Culture)), 11, "TextSecondary"));
        var note = Text(UiText.Get("AiUsageUnofficialNote"), 10.5, "TextSecondary", new Thickness(0, 1, 0, 0));
        note.TextWrapping = TextWrapping.Wrap;
        notes.Children.Add(note);
        footer.Children.Add(notes);
        return footer;
    }

    private static UIElement ValueLine(string label, string value)
    {
        var line = new DockPanel();
        var number = Text(value, 12, "TextPrimary");
        number.FontWeight = FontWeights.SemiBold;
        Typography.SetNumeralAlignment(number, FontNumeralAlignment.Tabular);
        DockPanel.SetDock(number, Dock.Right);
        line.Children.Add(number);
        var name = Text(label, 12, "TextPrimary");
        name.TextTrimming = TextTrimming.CharacterEllipsis;
        line.Children.Add(name);
        return line;
    }

    /// <summary>A usage bar; the optional pace tick marks where usage would be at an even rate through the window.</summary>
    private static UIElement Bar(double value, string accent, double? pace)
    {
        var bar = new Grid { Height = 8, Margin = new Thickness(0, 3, 0, 2) };
        var track = new Border { Height = 4, CornerRadius = new CornerRadius(2) };
        track.SetResourceReference(Border.BackgroundProperty, "TextMuted");
        bar.Children.Add(track);
        var fill = new Border { Height = 4, CornerRadius = new CornerRadius(2) };
        fill.SetResourceReference(Border.BackgroundProperty, accent);
        bar.Children.Add(Split(value, fill, 0));
        if (pace is { } elapsed)
        {
            var tick = new Border { Width = 2, CornerRadius = new CornerRadius(1), HorizontalAlignment = HorizontalAlignment.Left,
                Margin = new Thickness(-1, 0, 0, 0) };
            tick.SetResourceReference(Border.BackgroundProperty, "TextPrimary");
            bar.Children.Add(Split(elapsed, tick, 1));
            bar.ToolTip = UiText.Get("AiUsagePaceMarker");
            bar.Background = Brushes.Transparent;
        }
        return bar;
    }

    private static Grid Split(double fraction, UIElement child, int column)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(fraction, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1 - fraction, GridUnitType.Star) });
        Grid.SetColumn(child, column);
        grid.Children.Add(child);
        return grid;
    }

    private static Border Divider()
    {
        var divider = new Border { Height = 1, Margin = new Thickness(0, 2, 0, 10) };
        divider.SetResourceReference(Border.BackgroundProperty, "MenuDivider");
        return divider;
    }

    private static TextBlock Text(string text, double size, string brush, Thickness margin = default)
    {
        var block = new TextBlock { Text = text, FontSize = size, Margin = margin, VerticalAlignment = VerticalAlignment.Center };
        block.SetResourceReference(TextBlock.ForegroundProperty, brush);
        return block;
    }

    private Button IconButton(string icon, string labelKey, double size, Action click)
    {
        var glyph = new ContentControl { Content = TryFindResource(icon), Width = size, Height = size };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextSecondary");
        var button = new Button { Content = glyph, ToolTip = UiText.Get(labelKey) };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        AutomationProperties.SetName(button, UiText.Get(labelKey));
        button.Click += (_, _) => click();
        return button;
    }

    private static void OpenDashboard(AiProviders provider)
    {
        try
        {
            var url = provider switch
            {
                AiProviders.Claude => "https://claude.ai/settings/usage",
                AiProviders.Codex => "https://chatgpt.com/codex/settings/usage",
                AiProviders.OpenCode => "https://opencode.ai/go",
                AiProviders.Copilot => "https://github.com/settings/copilot",
                AiProviders.DeepSeek => "https://platform.deepseek.com/usage",
                AiProviders.OpenRouter => "https://openrouter.ai/settings/credits",
                _ => "https://antigravity.google/docs"
            };
            Process.Start(new ProcessStartInfo(url) { UseShellExecute = true });
        }
        catch (Exception ex) when (ex is System.ComponentModel.Win32Exception or InvalidOperationException)
        { AppLog.Info($"AI usage ({provider}) dashboard unavailable: {ex.GetType().Name}"); }
    }
}
