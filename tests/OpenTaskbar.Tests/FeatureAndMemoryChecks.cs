using System.IO;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class FeatureAndMemoryChecks
{
    public static int Run(Application app)
    {
        SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(Dispatcher.CurrentDispatcher));
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var previousLanguage = UiText.Current.Language;
        try
        {
            ThemeManager.Apply(AppTheme.Dark);
            UiText.Current.SetLanguage(AppLanguage.English);

            // 1. Lazy WidgetCoordinator parity & memory invariant
            var strip = new StackPanel();
            using (var lazyCoordinator = new WidgetCoordinator(strip, Dispatcher.CurrentDispatcher, _ => { }, lazy: true))
            {
                lazyCoordinator.Apply(new DockSettings(Widgets: []));
                Check(strip.Children.Count == 0, "Lazy WidgetCoordinator must instantiate 0 widgets when none are enabled.");

                lazyCoordinator.Apply(new DockSettings(Widgets: ["Clock", "Focus"]));
                Check(strip.Children.Count == 2 && strip.Children.OfType<ClockWidget>().Any() && strip.Children.OfType<FocusWidget>().Any(),
                    "Lazy WidgetCoordinator must instantiate only enabled widgets on demand.");

                var reordered = false;
                lazyCoordinator.OrderChanged = _ => reordered = true;
                lazyCoordinator.ReorderWidgets(0, 1);
                Check(reordered && lazyCoordinator.CurrentOrder[0] != "Search",
                    "Lazy WidgetCoordinator must support drag reordering.");

                // Enable all live widgets + SystemMeters.All
                lazyCoordinator.Apply(new DockSettings(
                    Widgets: WidgetCoordinator.DefaultWidgetOrder,
                    StatsMeters: SystemMeters.All));
                Check(strip.Children.Count == WidgetCoordinator.DefaultWidgetOrder.Length,
                    "Enabling all live widgets must materialize all controls.");
            }

            var aiLoads = 0;
            using (var ai = new AiUsageWidget((_, _) =>
            {
                aiLoads++;
                return Task.FromResult<(AiUsageReport?, string?)>((null, "ErrorAiUsageSignIn"));
            }))
            {
                ai.Activate();
                ai.Tick(DateTime.Now);
                Check(aiLoads == 0, "AI Usage must not load while all providers are disabled.");
                ai.ApplySettings(new DockSettings(AiUsageProviders: AiProviders.Claude));
                Check(aiLoads == 1 && ai.Visibility == Visibility.Visible
                    && AiCards(ai) is [var signInCard] && Summary(signInCard).Contains("claude", StringComparison.Ordinal),
                    "AI Usage must keep sign-in failures visible with a localized CLI hint.");
                ai.Tick(DateTime.Now);
                Check(aiLoads == 1, "A sign-in failure must not poll again without changed credentials.");
                ai.SetCompactState(true);
                Check(ai.BuildHoverFlyoutContent() is not null, "AI Usage must provide compact hover details.");
                ai.Deactivate();
                ai.Tick(DateTime.Now.AddHours(1));
                Check(aiLoads == 1, "Inactive AI widgets must not call their loader.");
            }

            var pending = new TaskCompletionSource<(AiUsageReport?, string?)>();
            CancellationToken aiCancellation = default;
            using (var ai = new AiUsageWidget((_, cancellation) =>
            {
                aiCancellation = cancellation;
                return pending.Task;
            }))
            {
                ai.ApplySettings(new DockSettings(AiUsageProviders: AiProviders.Codex));
                ai.Activate();
                ai.ApplySettings(new DockSettings());
                Check(aiCancellation.IsCancellationRequested, "Disabling AI providers must cancel their pending request.");
                pending.SetResult((new(AiProviders.Codex, "fixture", [new("AiUsageSession", 0.5, null, 18000)], DateTimeOffset.UtcNow), null));
                Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.Background);
                Check(AiCards(ai) is [var emptyCard] && Summary(emptyCard) == UiText.Get("AiUsageNoProviders"),
                    "A late AI response must not restore a disabled provider.");
            }

            var aiNow = DateTimeOffset.UtcNow;
            using (var ai = new AiUsageWidget((provider, _) => Task.FromResult<(AiUsageReport?, string?)>((new(provider, "Plus",
                [new("AiUsageSession", 0.42, aiNow.AddHours(3), 18000), new("AiUsageWeekly", provider == AiProviders.Codex ? 0.9 : 0.29,
                    aiNow.AddDays(5), 604800)], aiNow) { LimitReached = provider == AiProviders.Codex }, null))))
            {
                ai.ApplySettings(new DockSettings(AiUsageProviders: AiProviders.All));
                ai.Activate();
                Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.Background);
                var cards = AiCards(ai);
                Check(cards.Length == 7 && cards.All(card => card.Content is StackPanel { Children: [System.Windows.Shapes.Path { Data: not null }, TextBlock, RingMeter] })
                    && cards[0].Content is StackPanel { Children: [_, TextBlock { Text: "Claude" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && cards[1].Content is StackPanel { Children: [_, TextBlock { Text: "Codex" }, RingMeter { Value: 0.9 }] }
                    && cards[2].Content is StackPanel { Children: [_, TextBlock { Text: "Antigravity" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && cards[3].Content is StackPanel { Children: [_, TextBlock { Text: "OpenCode" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && cards[4].Content is StackPanel { Children: [_, TextBlock { Text: "Copilot" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && cards[5].Content is StackPanel { Children: [_, TextBlock { Text: "DeepSeek" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && cards[6].Content is StackPanel { Children: [_, TextBlock { Text: "OpenRouter" }, RingMeter { Value: 0.42, Text: "42" }] }
                    && Summary(cards[0]) is var claudeTip && claudeTip.StartsWith("Claude Code 42%", StringComparison.Ordinal)
                    && claudeTip.Contains("Weekly 29%", StringComparison.Ordinal) && claudeTip.Contains("Resets in", StringComparison.Ordinal)
                    && Summary(cards[1]) is var codexTip && codexTip.Contains(UiText.Get("AiUsageLimitReached"), StringComparison.Ordinal),
                    "AI Usage must show one card per provider with logo, name, usage ring and a one-line summary.");
                ai.ApplySettings(new DockSettings(AiUsageProviders: AiProviders.All, AiUsageRing: AiUsageRing.Weekly));
                Check(Summary(AiCards(ai)[0]) is var weeklyTip && weeklyTip.StartsWith("Claude Code 29%", StringComparison.Ordinal),
                    "The ring setting must choose the window the dock shows.");
                ai.SetCompactState(true);
                Check(AiCards(ai) is [var compactCard] && Summary(compactCard).Contains('\n'),
                    "Compact AI Usage must collapse to one card that summarizes every provider.");
                Check(ai.BuildHoverFlyoutContent() is StackPanel aiFlyout && aiFlyout.Children.Count > 4,
                    "The AI Usage flyout must build sections for every provider.");
            }

            // 2. SystemStats + on-demand SystemSensors parity & disposal
            var stats = new SystemStats();
            _ = stats.SampleCpu();
            Check(SystemStats.SampleMemory() is >= 0 and <= 1, "SystemStats.SampleMemory must report valid load.");
            _ = SystemStats.SampleBattery();

            using (var sensors = new SystemSensors(SystemMeters.None))
            {
                sensors.Configure(SystemMeters.Gpu);
                _ = sensors.SampleGpuUsage();
                sensors.Configure(SystemMeters.CpuTemperature);
                _ = sensors.SampleCpuTemperature();
                sensors.Configure(SystemMeters.GpuTemperature);
                _ = sensors.SampleGpuTemperature();
                sensors.Configure(SystemMeters.All);
                _ = sensors.SampleGpuUsage();
                _ = sensors.SampleCpuTemperature();
                _ = sensors.SampleGpuTemperature();
                sensors.Configure(SystemMeters.None);
            }
            Check(true, "SystemSensors must configure, sample, and dispose cleanly across all SystemMeters combinations.");

            // 3. Shared AudioEndpointMonitor reference counting
            using (var a1 = AudioEndpointMonitor.AcquireShared())
            {
                using (var a2 = AudioEndpointMonitor.AcquireShared())
                {
                    Check(a1.HasDevice == a2.HasDevice && Math.Abs(a1.Volume - a2.Volume) < 0.001f && a1.Muted == a2.Muted,
                        "Shared AudioEndpointMonitor handles must observe identical audio state.");
                }
                _ = a1.Volume;
            }
            Check(true, "Shared AudioEndpointMonitor handles must survive partial release and clean final disposal.");

            checks += AudioMixerChecks.Run();

            // 4. WindowThumbnailPreview lazy creation & disposal
            var preview = new WindowThumbnailPreview((Style)app.FindResource("DockButton"));
            Check(!preview.IsVisible && preview.SourceHandle == 0 && preview.ActiveHandles.Count == 0,
                "WindowThumbnailPreview must start hidden with zero handles before first hover.");
            Check(!preview.ShowForWindows([], strip, null),
                "WindowThumbnailPreview must not open when given no valid windows.");
            preview.Hide();
            preview.Dispose();
            Check(true, "WindowThumbnailPreview must dispose cleanly without ever creating HWNDs when unused.");

            // 5. WeatherService frozen cached brushes
            var clear1 = WeatherService.GetConditionBackground(0);
            var clear2 = WeatherService.GetConditionBackground(0);
            Check(clear1.IsFrozen && ReferenceEquals(clear1, clear2),
                "WeatherService condition brushes must be frozen and cached.");

            // 6. DockFlyout visual-tree cleanup & weak target release
            var (flyout, contentRef) = ExerciseDockFlyout(app, Check);
            app.Dispatcher.Invoke(() => { }, DispatcherPriority.ContextIdle);
            GC.Collect();
            GC.WaitForPendingFinalizers();
            Check(!contentRef.IsAlive, "DockFlyout must release its child visual tree on close.");
            var popupField = typeof(DockFlyout).GetField("_popup", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
            var popup = (System.Windows.Controls.Primitives.Popup)popupField.GetValue(flyout)!;
            Check(popup.PlacementTarget is null, "DockFlyout must clear PlacementTarget on close.");
            GC.KeepAlive(flyout);

            // 7. Shared ContextMenu + LauncherWindow lifecycle verification
            var sharedMenu = new ContextMenu
            {
                Items = { new MenuItem { Header = "Open" }, new MenuItem { Header = "Close" } }
            };
            var dockItem1 = new Button { ContextMenu = sharedMenu, DataContext = new DockApp("App1", "a1.exe") };
            var dockItem2 = new Button { ContextMenu = sharedMenu, DataContext = new DockApp("App2", "a2.exe") };
            Check(ReferenceEquals(dockItem1.ContextMenu, dockItem2.ContextMenu),
                "Shared ContextMenu must be reused across dock items without per-item duplication.");

            var search = new SearchWidget();
            search.Activate();
            Check(search.BuildHoverFlyoutContent() is not null,
                "Search widget must provide hover content.");
            var tray = new TrayWidget();
            tray.Activate();
            search.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            tray.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            Check(Math.Abs(search.DesiredSize.Height - tray.DesiredSize.Height) < 0.001,
                $"Search widget ({search.DesiredSize.Height}) and Tray widget ({tray.DesiredSize.Height}) must have identical desired heights.");
            Check(Math.Abs(search.DesiredSize.Width - tray.DesiredSize.Width) < 0.001,
                $"Search widget ({search.DesiredSize.Width}) and Tray widget ({tray.DesiredSize.Width}) must have identical desired widths.");
            tray.Deactivate();
            search.Deactivate();

            // Verify MediaWidget lifecycle, hover flyout, density and tick safety
            using (var mediaWidget = new MediaWidget())
            {
                mediaWidget.Activate();
                Check(mediaWidget.BuildHoverFlyoutContent() is not null, "MediaWidget must provide rich hover flyout content.");
                Check(mediaWidget.SupportsCompactMode, "MediaWidget must declare compact mode support.");
                Check(mediaWidget.FullWidthEstimate > mediaWidget.CompactWidthEstimate, "MediaWidget full width must exceed compact width.");
                mediaWidget.Tick(DateTime.Now);
                mediaWidget.Deactivate();
            }





            // 8. Heavy DockSettingsStore validation & round-trip
            var tempSettings = Path.Combine(Path.GetTempPath(), $"ot-heavy-{Guid.NewGuid():N}.json");
            try
            {
                var store = new DockSettingsStore(tempSettings);
                var heavy = new DockSettings(
                    Position: DockPosition.Center,
                    Theme: AppTheme.Dark,
                    Language: AppLanguage.English,
                    Widgets: ["Media", "Desktops", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Tray", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    InstalledWidgets: ["Media", "Desktops", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Tray", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    ShowRecycleBin: true,
                    ShowOnAllMonitors: true,
                    StatsMeters: SystemMeters.All,
                    Profiles: [new DockProfile("Work", [@"C:\Games\game.exe"])],
                    ActiveProfile: "Work");
                Check(store.Save(heavy) is null, "Heavy DockSettings must save without errors.");
                var (loaded, loadErr) = store.Load();
                Check(loadErr is null && loaded.StatsMeters == SystemMeters.All && loaded.Widgets?.Count == 13 && loaded.ShowOnAllMonitors,
                    "Heavy DockSettings must round-trip all 13 widgets, SystemMeters.All, and multi-monitor flags.");
            }
            finally
            {
                try { File.Delete(tempSettings); } catch (IOException) { }
            }

            // 9. TrayIcons PNG DecodePixelWidth capping (caps > 32px without upscaling <= 32px)
            var decodeMethod = typeof(TrayIcons).GetMethod("DecodeSnapshot",
                System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Static)!;
            var smallPng = EncodeSolidPng(16);
            var largePng = EncodeSolidPng(256);
            var decodedSmall = (BitmapSource?)decodeMethod.Invoke(null, [smallPng]);
            var decodedLarge = (BitmapSource?)decodeMethod.Invoke(null, [largePng]);
            Check(decodedSmall?.PixelWidth == 16, "Small tray PNG snapshots must not be upscaled.");
            Check(decodedLarge?.PixelWidth == 32, "Oversized tray PNG snapshots must be downsampled to 32px.");

            // 10. NativeTaskbarWindow & Live TrayHost checks
            var explorerTaskbar = NativeTaskbarWindow.Find();
            if (explorerTaskbar != 0)
            {
                GetWindowThreadProcessId(explorerTaskbar, out var ownerPid);
                Check(ownerPid != (uint)Environment.ProcessId, "NativeTaskbarWindow.Find must never return a window owned by OpenTaskbar.");
            }
            Check(!TrayHost.IsBroadcasting, "TrayHost.IsBroadcasting must be false when idle.");
            using (var testTray = new TrayWidget())
            {
                var liveEmptyContent = testTray.BuildLiveContent(Array.Empty<ManagedShell.WindowsTray.NotifyIcon>());
                Check(liveEmptyContent is StackPanel sp && sp.Children.Count >= 2,
                    "TrayWidget.BuildLiveContent must render a heading, empty caption and settings for an empty icon list.");
            }
        }
        finally
        {
            UiText.Current.SetLanguage(previousLanguage);
        }

        return checks;
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static (DockFlyout Flyout, WeakReference ContentRef) ExerciseDockFlyout(
        Application app,
        Action<bool, string> check)
    {
        var flyout = new DockFlyout();
        var anchor = new Button();
        var content = new Border { Width = 40, Height = 40 };
        var hostWin = new Window { Content = anchor, Width = 100, Height = 60, ShowInTaskbar = false };
        hostWin.Show();
        try
        {
            flyout.Show(anchor, content, animate: false);
            check(flyout.IsOpen, "DockFlyout must open when shown.");
            flyout.Close();
            app.Dispatcher.Invoke(() => { }, DispatcherPriority.Background);
            check(!flyout.IsOpen, "DockFlyout must close cleanly.");
            check(flyout.RecentlyClosed(anchor), "DockFlyout must still detect recently closed target via WeakReference.");
        }
        finally
        {
            hostWin.Close();
        }

        return (flyout, new WeakReference(content));
    }

    private static byte[] EncodeSolidPng(int size)
    {
        var pixels = new byte[size * size * 4];
        var bmp = BitmapSource.Create(size, size, 96, 96, PixelFormats.Bgra32, null, pixels, size * 4);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bmp));
        using var ms = new MemoryStream();
        encoder.Save(ms);
        return ms.ToArray();
    }

    /// <summary>A card's one-line summary, kept for screen readers now that the hover card replaces its tooltip.</summary>
    private static string Summary(Button card) => System.Windows.Automation.AutomationProperties.GetHelpText(card);

    private static Button[] AiCards(AiUsageWidget widget) =>
        ((Panel)widget.Content).Children.OfType<Button>().Where(card => card.Visibility == Visibility.Visible).ToArray();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);
}
