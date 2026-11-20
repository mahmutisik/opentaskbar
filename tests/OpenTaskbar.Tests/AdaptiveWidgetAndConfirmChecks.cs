using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class AdaptiveWidgetAndConfirmChecks
{
    public static int Run(Application application)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckTwoStepConfirm(application.Dispatcher, Check);
        CheckPreviewDimensions(application, Check);
        CheckDensityPersistence(Check);
        CheckDockItemOrderPersistence(Check);
        CheckAdaptiveDensityAndHysteresis(application.Dispatcher, Check);
        CheckScrollableAppBudget(Check);
        CheckVisualBalanceAndTrayFiltering(application, Check);
        CheckEnhancedAudioCenter(application, Check);
        return checks;
    }

    private static void CheckTwoStepConfirm(Dispatcher dispatcher, Action<bool, string> check)
    {
        long fakeNow = 1000;
        var armed = 0;
        var disarmed = 0;
        var key = new object();
        using var confirm = new TwoStepConfirm(dispatcher, windowMs: 2500, clockMs: () => fakeNow);

        var first = confirm.ConsumeOrArm(key, () => armed++, () => disarmed++);
        check(!first && armed == 1 && confirm.IsArmed(key),
            "First click must arm confirmation without executing immediately.");

        fakeNow += 1200;
        var second = confirm.ConsumeOrArm(key, () => armed++, () => disarmed++);
        check(second && armed == 1 && disarmed == 1 && !confirm.IsArmed(key),
            "Second click within confirmation window must execute and disarm.");

        fakeNow += 100;
        var rearmed = confirm.ConsumeOrArm(key, () => armed++, () => disarmed++);
        check(!rearmed && armed == 2 && confirm.IsArmed(key),
            "Subsequent click after execution must require arming again.");

        fakeNow += 3000;
        check(!confirm.IsArmed(key), "Confirmation arm state must expire after the window elapses.");
        var expiredClick = confirm.ConsumeOrArm(key, () => armed++, () => disarmed++);
        check(!expiredClick && armed == 3, "Click after expiration must re-arm rather than execute.");
    }

    private static void CheckPreviewDimensions(Application application, Action<bool, string> check)
    {
        var width = (double)application.FindResource("PreviewWidth");
        var height = (double)application.FindResource("PreviewHeight");
        check(width <= 240 && height <= 160,
            $"Window thumbnail previews must stay compact (got {width}x{height}).");
    }

    private static void CheckDensityPersistence(Action<bool, string> check)
    {
        var path = Path.Combine(Path.GetTempPath(), $"opentaskbar-density-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(path);
            var settings = new DockSettings(
                GlobalWidgetDensity: WidgetDensityMode.Compact,
                WidgetDensityOverrides: new Dictionary<string, WidgetDensityMode>
                {
                    ["Audio"] = WidgetDensityMode.Full,
                    ["SystemStats"] = WidgetDensityMode.Auto
                });
            check(store.Save(settings) is null, "Density settings must save cleanly.");
            var loaded = store.Load();
            check(loaded.Error is null
                && loaded.Settings.GlobalWidgetDensity == WidgetDensityMode.Compact
                && loaded.Settings.WidgetDensityOverrides?["Audio"] == WidgetDensityMode.Full
                && loaded.Settings.WidgetDensityOverrides?["SystemStats"] == WidgetDensityMode.Auto,
                "Global and per-widget density settings must persist and reload accurately.");
        }
        finally
        {
            File.Delete(path);
            File.Delete(path + ".tmp");
        }
    }

    private static void CheckDockItemOrderPersistence(Action<bool, string> check)
    {
        var path = Path.Combine(Path.GetTempPath(), $"opentaskbar-dock-order-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(path);
            var order = new[] { "widget:Media", "start", "app:C:\\Tools\\app.exe", "stack:C:\\Docs", "bin" };
            check(store.Save(new DockSettings(DockItemOrder: order)) is null,
                "Free-form dock order must save cleanly.");
            var loaded = store.Load();
            check(loaded.Error is null && loaded.Settings.DockItemOrder?.SequenceEqual(order) == true,
                "Free-form dock order must round-trip exactly.");
        }
        finally
        {
            File.Delete(path);
            File.Delete(path + ".tmp");
        }
    }

    private static void CheckAdaptiveDensityAndHysteresis(Dispatcher dispatcher, Action<bool, string> check)
    {
        var strip = new StackPanel { Orientation = Orientation.Horizontal };
        using var coordinator = new WidgetCoordinator(strip, dispatcher, _ => { }, lazy: false);
        var widgets = new[] { "Search", "Media", "QuickSettings", "SystemStats", "Audio", "Focus", "Desktops", "Clock" };
        var settings = new DockSettings(
            Widgets: widgets,
            InstalledWidgets: widgets,
            GlobalWidgetDensity: WidgetDensityMode.Auto);
        coordinator.Apply(settings);

        var audio = strip.Children.OfType<AudioWidget>().Single();
        var qs = strip.Children.OfType<QuickSettingsWidget>().Single();
        var autoCandidates = strip.Children.OfType<OpenTaskbar.Widgets.DockWidget>()
            .Where(widget => widget.Visibility == Visibility.Visible && widget.SupportsCompactMode)
            .OrderBy(widget => widget.ShrinkPriority).ToArray();

        check(autoCandidates.Length > 0,
            "The widget strip must contain at least one compact-capable widget.");

        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 2000, appsContentWidth: 400, settings);
        check(autoCandidates.All(widget => !widget.IsCompact),
            "Auto widgets must stay full when there is enough room.");

        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 650, appsContentWidth: 720, settings);
        check(autoCandidates[0].IsCompact,
            "Auto density must compact the highest-priority eligible widget when space runs out.");
        check(autoCandidates.Skip(1).All(widget => !widget.IsCompact),
            "Auto density must leave other widgets full when one compaction is enough.");

        var saving = autoCandidates[0].FullWidthEstimate - autoCandidates[0].CompactWidthEstimate;
        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 650, appsContentWidth: 720 - saving, settings);
        check(autoCandidates[0].IsCompact,
            "Auto density must not oscillate after measuring compact widgets.");
        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 730, appsContentWidth: 720 - saving, settings);
        check(autoCandidates[0].IsCompact,
            "Hysteresis must prevent immediate expansion near the width limit.");
        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 800, appsContentWidth: 720 - saving, settings);
        check(autoCandidates.All(widget => !widget.IsCompact),
            "Auto density must expand widgets when enough space returns.");

        var overridden = settings with
        {
            WidgetDensityOverrides = new Dictionary<string, WidgetDensityMode>
            {
                ["Audio"] = WidgetDensityMode.Compact,
                ["QuickSettings"] = WidgetDensityMode.Full
            }
        };
        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 2000, appsContentWidth: 400, overridden);
        check(audio.IsCompact && !qs.IsCompact,
            "Per-widget Compact/Full choices must override Auto.");

        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 650, appsContentWidth: 720, settings, widgetScale: 1.35);
        check(autoCandidates[0].IsCompact,
            "Scaled Auto widgets must compact when the dock runs out of room.");
        coordinator.UpdateAdaptiveDensity(maxTotalWidth: 2000, appsContentWidth: 400, settings);
        check(autoCandidates.All(widget => !widget.IsCompact),
            "Auto widgets must return to full size when there is enough room.");
        check(!coordinator.TryCompactNextAutoWidget(settings),
            "Single-step compaction is not used; density is recalculated for the whole strip.");
    }

    private static void CheckScrollableAppBudget(Action<bool, string> check)
    {
        const double floor = 104;

        check(WidgetCoordinator.EffectiveAppsWidth(600, 400, floor) == 400,
            "Apps that fit without scrolling must keep their full width in density calculations.");
        check(WidgetCoordinator.EffectiveAppsWidth(600, 160, floor) == 160,
            "Scrollable app overflow must not compact widgets while apps still have more than the reserved floor.");
        check(WidgetCoordinator.EffectiveAppsWidth(600, 0, floor) == floor,
            "Once apps would be squeezed below the reserved floor, the remaining overflow must reach widget density.");
        check(WidgetCoordinator.EffectiveAppsWidth(60, 0, floor) == 60,
            "A short app strip must never be padded up to the reserved floor.");
    }

    private static void CheckVisualBalanceAndTrayFiltering(Application application, Action<bool, string> check)
    {
        var startLogoSize = application.TryFindResource("StartLogoSize");
        check(startLogoSize is double d && d > 0,
            "Theme resources must expose StartLogoSize for proportional scaling across dock sizes.");

        foreach (var dockSize in new[] { DockSize.Compact, DockSize.Medium, DockSize.Large })
        {
            var p = DockMetrics.For(dockSize);
            check(p.Icon * DockMetrics.HoverScale < p.Cell,
                $"Magnified icons in {dockSize} mode must fit inside cell spacing without overlapping adjacent apps.");
        }

        using var hover = new WidgetHoverCoordinator(application.Dispatcher, _ => false, _ => { });
        using var qsWidget = new QuickSettingsWidget();
        using var audioWidget = new AudioWidget();
        hover.Attach(qsWidget);
        hover.Attach(audioWidget);
        check(!hover.IsHoverActive, "WidgetHoverCoordinator must remain inactive at rest.");
        check(qsWidget.BuildHoverFlyoutContent() is not null && audioWidget.BuildHoverFlyoutContent() is not null,
            "Compact-capable widgets must produce hover flyout content cleanly.");
        hover.CollapseActive();
        check(!hover.IsHoverActive, "CollapseActive must reset hover active state.");

        // Full-size widgets show their click flyout on hover; the ones that already show everything inline show nothing.
        using var clock = new ClockWidget();
        using var weather = new WeatherWidget();
        using var netSpeed = new NetSpeedWidget();
        using var bluetooth = new BluetoothBatteryWidget();
        using var aiUsage = new AiUsageWidget();
        foreach (var widget in new OpenTaskbar.Widgets.DockWidget[] { clock, weather, netSpeed, bluetooth, aiUsage, audioWidget })
            check(widget.BuildFullHoverFlyoutContent() is not null,
                $"{widget.GetType().Name} must show its flyout when the pointer rests on the full-size widget.");
        check(qsWidget.BuildFullHoverFlyoutContent() is null,
            "A full-size widget that already shows its controls inline must not open a hover card.");
    }

    private static void CheckEnhancedAudioCenter(Application application, Action<bool, string> check)
    {
        // 1. Localization keys for enhanced audio controls in Turkish and English
        UiText.Current.SetLanguage(AppLanguage.Turkish);
        check(!string.IsNullOrWhiteSpace(UiText.Get("AudioOutputDevices")), "AudioOutputDevices key must exist in Turkish.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("AudioInputDevices")), "AudioInputDevices key must exist in Turkish.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("Microphone")), "Microphone key must exist in Turkish.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("MicMuteToggle")), "MicMuteToggle key must exist in Turkish.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("AppVolumeMixer")), "AppVolumeMixer key must exist in Turkish.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("NoActiveAudioApps")), "NoActiveAudioApps key must exist in Turkish.");

        UiText.Current.SetLanguage(AppLanguage.English);
        check(!string.IsNullOrWhiteSpace(UiText.Get("AudioOutputDevices")), "AudioOutputDevices key must exist in English.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("AudioInputDevices")), "AudioInputDevices key must exist in English.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("Microphone")), "Microphone key must exist in English.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("MicMuteToggle")), "MicMuteToggle key must exist in English.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("AppVolumeMixer")), "AppVolumeMixer key must exist in English.");
        check(!string.IsNullOrWhiteSpace(UiText.Get("NoActiveAudioApps")), "NoActiveAudioApps key must exist in English.");

        // 2. Data contracts
        var dev = new AudioDeviceInfo("dev-1", "Test Headphones", true, false);
        check(dev.Id == "dev-1" && dev.Name == "Test Headphones" && dev.IsDefault && !dev.IsInput,
            "AudioDeviceInfo contract must retain device identity, state, and flow.");

        var volumeSet = 0f;
        var muteSet = false;
        var session = new AppAudioSession(1234, "Spotify", null, 0.75f, false,
            vol => { volumeSet = vol; return true; },
            m => { muteSet = m; return true; });
        check(session.ProcessId == 1234 && session.Name == "Spotify" && Math.Abs(session.Volume - 0.75f) < 0.001f && !session.IsMuted,
            "AppAudioSession contract must retain session metadata and initial state.");
        session.SetVolume(0.5f);
        session.SetMute(true);
        check(Math.Abs(volumeSet - 0.5f) < 0.001f && muteSet,
            "AppAudioSession delegates must successfully propagate volume and mute commands.");

        // 3. Audio widget flyout construction
        using var audioWidget = new AudioWidget();
        var flyout = audioWidget.BuildHoverFlyoutContent();
        check(flyout is Border container && container.Child is StackPanel root && root.Children.Count >= 4,
            "AudioWidget flyout must build a multi-section audio center container.");
    }
}
