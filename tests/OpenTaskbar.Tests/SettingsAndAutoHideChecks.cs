using System.Globalization;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class SettingsAndAutoHideChecks
{
    public static int Run(Application application)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckSettingsSurviveReopen(application, Check);
        CheckAutoHideLifecycleAndPointerSync(application, Check);
        CheckFlyoutState(application, Check);
        CheckClockWidgetFlyoutAndAutoHide(application, Check);
        CheckTrayTransientTooltips(Check);
        return checks;
    }

    private static void CheckFlyoutState(Application application, Action<bool, string> check)
    {
        var anchor = new Button { Content = "Flyout anchor" };
        var host = new Window
        {
            Width = 300, Height = 180, Left = -20000, Top = -20000,
            ShowInTaskbar = false, ShowActivated = false, Content = anchor
        };
        var flyout = new DockFlyout();
        var secondFlyout = new DockFlyout();
        try
        {
            host.Show();
            PumpDispatcher(application.Dispatcher, 100);
            flyout.Show(anchor, new TextBlock { Text = "Calendar" }, animate: false, focusable: false);
            PumpDispatcher(application.Dispatcher, 100);
            check(flyout.IsOpen && DockFlyout.IsAnyOpen,
                "An open widget flyout must lock auto-hide.");

            flyout.Close();
            PumpDispatcher(application.Dispatcher, 100);
            check(!flyout.IsOpen && !DockFlyout.IsAnyOpen,
                "Closing the widget flyout must release the auto-hide lock.");

            flyout.Show(anchor, new TextBlock { Text = "Weather" }, animate: false, focusable: false);
            PumpDispatcher(application.Dispatcher, 100);
            secondFlyout.Show(anchor, new TextBlock { Text = "Tray" }, animate: false, focusable: false);
            PumpDispatcher(application.Dispatcher, 100);
            check(!flyout.IsOpen && secondFlyout.IsOpen && DockFlyout.IsAnyOpen,
                "Opening another widget flyout must close the old one without releasing the new lock.");
            DockFlyout.CloseAll();
            PumpDispatcher(application.Dispatcher, 100);
            check(!flyout.IsOpen && !secondFlyout.IsOpen && !DockFlyout.IsAnyOpen,
                "Closing all flyouts must release the auto-hide lock after switching widgets.");
        }
        finally
        {
            flyout.Close();
            secondFlyout.Close();
            host.Close();
        }
    }

    private static void CheckTrayTransientTooltips(Action<bool, string> check)
    {
        check(TrayIcons.IsTransientTooltip("Kaspersky Premium 21.27.6.532\r\nStarting...")
            && TrayIcons.IsTransientTooltip("Microsoft 365 and Office are installing in the background.")
            && TrayIcons.IsTransientTooltip("Uygulama başlatılıyor..."),
            "Transient startup/installer tray tooltips must fall back to live executable icons.");
        check(!TrayIcons.IsTransientTooltip("Everything") && !TrayIcons.IsTransientTooltip("NVIDIA Ayarları"),
            "Resting tray tooltips must keep their tray icon snapshots.");
    }

    private static void CheckSettingsSurviveReopen(Application application, Action<bool, string> check)
    {
        var path = Path.Combine(Path.GetTempPath(), $"opentaskbar-settings-reopen-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(path, CultureInfo.GetCultureInfo("tr-TR"));
            var initial = new DockSettings(
                Language: AppLanguage.Turkish,
                Theme: AppTheme.Dark,
                AutoHide: AutoHideMode.Smart,
                StartPlacement: StartButtonPlacement.End,
                Size: DockSize.Compact,
                SurfaceOpacity: 0.6,
                DockBlur: false,
                LauncherHotkey: HotkeyChoice.CtrlSpace,
                DockHotkey: HotkeyChoice.WinAltD,
                Widgets: ["Clock", "Media", "SystemStats"],
                InstalledWidgets: ["Clock", "Media", "SystemStats"],
                ThemeApplicationIcons: false);
            check(store.Save(initial) is null, "Initial custom dock settings must save cleanly.");

            var preferences = new ApplicationPreferences(store);
            var changeCount = 0;
            preferences.Changed += (_, _) => changeCount++;

            var sw1 = CreateOffscreenSettings(preferences);
            sw1.Show();
            PumpDispatcher(application.Dispatcher, 120);
            check(changeCount == 0, "Opening SettingsWindow must not mutate stored preferences.");

            sw1.Close();
            PumpUntil(application.Dispatcher, () => sw1.IsClosingOrClosed && !sw1.IsVisible, 1200);
            PumpDispatcher(application.Dispatcher, 220);

            var sw2 = CreateOffscreenSettings(preferences);
            try
            {
                sw2.Show();
                PumpDispatcher(application.Dispatcher, 120);
                if (FindFirstVisual<TabControl>(sw2) is { } tabs)
                {
                    for (var i = 0; i < tabs.Items.Count; i++)
                    {
                        tabs.SelectedIndex = i;
                        PumpDispatcher(application.Dispatcher, 30);
                    }
                }
            }
            finally
            {
                sw2.Close();
                PumpDispatcher(application.Dispatcher, 220);
            }

            var reloaded = store.Load();
            check(changeCount == 0,
                "Reopening SettingsWindow and browsing tabs after closing a prior instance must not fire spurious preference changes.");
            check(reloaded.Error is null, "Reloaded settings after SettingsWindow reopen must stay valid.");
            check(!reloaded.Settings.ThemeApplicationIcons,
                "Disabling themed application icons must survive settings reopen.");
            check(preferences.Current.AutoHide == AutoHideMode.Smart && reloaded.Settings.AutoHide == AutoHideMode.Smart,
                "AutoHide preference must not reset when SettingsWindow is reopened.");
            check(preferences.Current.StartPlacement == StartButtonPlacement.End && reloaded.Settings.StartPlacement == StartButtonPlacement.End,
                "StartPlacement preference must not reset when SettingsWindow is reopened.");
            check(preferences.Current.LauncherHotkey == HotkeyChoice.CtrlSpace && reloaded.Settings.LauncherHotkey == HotkeyChoice.CtrlSpace,
                "LauncherHotkey preference must not reset when SettingsWindow is reopened.");
            check(preferences.Current.DockHotkey == HotkeyChoice.WinAltD && reloaded.Settings.DockHotkey == HotkeyChoice.WinAltD,
                "DockHotkey preference must not reset when SettingsWindow is reopened.");
            check(preferences.Current.Widgets?.SequenceEqual(["Clock", "Media", "SystemStats"]) == true
                && reloaded.Settings.Widgets?.SequenceEqual(["Clock", "Media", "SystemStats"]) == true,
                "Enabled widgets must not be wiped when SettingsWindow is reopened.");
            check(preferences.Current.InstalledWidgets?.SequenceEqual(["Clock", "Media", "SystemStats"]) == true
                && reloaded.Settings.InstalledWidgets?.SequenceEqual(["Clock", "Media", "SystemStats"]) == true,
                "Installed widgets must not be wiped when SettingsWindow is reopened.");
            check(Math.Abs(loadedOpacity(reloaded.Settings) - 0.6) < 0.001,
                "SurfaceOpacity preference must survive SettingsWindow reopen.");
            check(!preferences.Current.DockBlur && !reloaded.Settings.DockBlur && new DockSettings().DockBlur,
                "The glass blur is on by default and a disabled blur must survive SettingsWindow reopen.");
        }
        finally
        {
            File.Delete(path);
            File.Delete(path + ".tmp");
        }

        static double loadedOpacity(DockSettings s) => s.SurfaceOpacity;

        static SettingsWindow CreateOffscreenSettings(ApplicationPreferences prefs) => new(prefs)
        {
            Left = -20000,
            Top = -20000,
            ShowActivated = false,
            ShowInTaskbar = false
        };
    }

    private static void CheckAutoHideLifecycleAndPointerSync(Application application, Action<bool, string> check)
    {
        var translate = new TranslateTransform();
        var surface = new Border { Width = 600, Height = 56, RenderTransform = translate };
        var host = new Window
        {
            Width = 600,
            Height = 76,
            Left = -20000,
            Top = -20000,
            ShowInTaskbar = false,
            ShowActivated = false,
            WindowStyle = WindowStyle.None,
            AllowsTransparency = true,
            Background = Brushes.Transparent,
            Content = surface
        };

        DockAutoHideController? controller = null;
        try
        {
            var hwnd = new WindowInteropHelper(host).EnsureHandle();
            host.Show();
            surface.Measure(new Size(600, 76));
            surface.Arrange(new Rect(0, 0, 600, 56));
            surface.UpdateLayout();

            var locked = true;
            var simulatedCursor = (X: 0, Y: 0);
            var waitBudgetMs = (int)DockMetrics.AutoHideDelayMs + 800;
            controller = new DockAutoHideController(host, hwnd, translate, surface, () => locked)
            {
                AnimationsEnabled = false,
                Edge = DockEdge.Bottom,
                DockBoundsProvider = () => (-10000, -10000, -9400, -9944),
                CursorPositionProvider = () => simulatedCursor
            };

            controller.SetMode(AutoHideMode.Always);
            PumpDispatcher(application.Dispatcher, (int)DockMetrics.AutoHideDelayMs + 150);
            check(!controller.IsHidden, "Interaction-locked dock must stay visible in auto-hide mode.");

            locked = false;
            controller.EvaluateState();
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden && translate.Y >= DockMetrics.HideMinimumOffset,
                "Unlocked dock in Always auto-hide mode must slide off-screen after delay.");

            controller.OnMouseEnterEdge();
            check(!controller.IsHidden && Math.Abs(translate.Y) < 0.001,
                "Hovering the screen trigger edge must reveal the hidden dock.");

            controller.OnMouseLeaveEdge();
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden, "Leaving the trigger edge must hide the dock again.");

            simulatedCursor = (-9700, -9970);
            controller.OnMouseEnterDock();
            check(!controller.IsHidden && Math.Abs(translate.Y) < 0.001,
                "Hovering the dock must reveal it.");
            controller.EvaluateState();
            check(controller.IsMouseOverDock && !controller.IsHidden,
                "EvaluateState must keep IsMouseOverDock while cursor stays inside dock bounds.");

            // Simulate missed MouseLeave once cursor moves outside (-10000, -10000, -9400, -9944).
            simulatedCursor = (0, 0);
            controller.EvaluateState();
            check(!controller.IsMouseOverDock,
                "EvaluateState must clear stuck IsMouseOverDock when cursor is outside dock bounds.");
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden,
                "Dock must hide once stuck IsMouseOverDock is cleared by pointer sync.");

            // Long hover beyond the old 4-tick limit (simulate hovering for 1.3s, then moving away without MouseLeave)
            simulatedCursor = (-9700, -9970);
            controller.OnMouseEnterDock();
            check(!controller.IsHidden, "Dock must reveal on mouse enter.");
            PumpDispatcher(application.Dispatcher, 1100);
            check(!controller.IsHidden && controller.IsMouseOverDock, "Dock must stay revealed during sustained hover.");
            // Cursor now moves away, but simulate missed MouseLeave (do NOT call controller.EvaluateState or OnMouseLeave)
            simulatedCursor = (0, 0);
            // The continuous guard timer must automatically detect cursor departure and hide the dock
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden, "Continuous guard timer must hide the dock after a long hover even if MouseLeave was dropped.");

            controller.SetMode(AutoHideMode.Smart);
            // Keep the same-process rule independent of whatever real window happens to be foreground on the CI box.
            controller.ForegroundProvider = () => hwnd;
            controller.CheckForegroundWindow(hwnd);
            // A hide tick queued by the previous phase can still run after the mode switch; let it settle.
            PumpUntil(application.Dispatcher, () => !controller.IsHidden, waitBudgetMs);
            check(!controller.HasOverlap && !controller.IsHidden,
                "Smart mode must ignore same-process foreground windows and stay visible without foreign overlap.");

            controller.SetMode(AutoHideMode.Disabled);
            check(!controller.IsHidden && Math.Abs(translate.Y) < 0.001,
                "Disabling auto-hide must keep dock revealed at zero offset.");
        }
        finally
        {
            controller?.Dispose();
            host.Close();
        }
    }

    private static void CheckClockWidgetFlyoutAndAutoHide(Application application, Action<bool, string> check)
    {
        var translate = new TranslateTransform();
        var clock = new ClockWidget();
        var surface = new Border
        {
            Width = 600,
            Height = 56,
            Child = clock,
            RenderTransform = translate
        };
        var host = new Window
        {
            Width = 600,
            Height = 100,
            Content = surface,
            ShowInTaskbar = false,
            WindowStyle = WindowStyle.None
        };

        DockAutoHideController? controller = null;
        try
        {
            var hwnd = new WindowInteropHelper(host).EnsureHandle();
            host.Show();
            surface.Measure(new Size(600, 76));
            surface.Arrange(new Rect(0, 0, 600, 56));
            surface.UpdateLayout();
            clock.Activate();

            var simulatedCursor = (X: -9700, Y: -9970);
            var waitBudgetMs = (int)DockMetrics.AutoHideDelayMs + 800;
            controller = new DockAutoHideController(host, hwnd, translate, surface, () => DockFlyout.IsAnyOpen)
            {
                AnimationsEnabled = false,
                Edge = DockEdge.Bottom,
                DockBoundsProvider = () => (-10000, -10000, -9400, -9944),
                CursorPositionProvider = () => simulatedCursor
            };
            controller.SetMode(AutoHideMode.Always);

            var clockBtn = (Button)clock.FindName("ClockButton");
            check(!DockFlyout.IsAnyOpen, "DockFlyout must start closed.");

            // 1. Click ClockButton to open calendar
            clockBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            PumpDispatcher(application.Dispatcher, 100);
            check(DockFlyout.IsAnyOpen, "ClockButton click must open the calendar flyout.");

            // Cursor moves away: flyout remains open and keeps dock visible until user dismisses or toggles
            simulatedCursor = (0, 0);
            controller.EvaluateState();
            PumpDispatcher(application.Dispatcher, 200);
            check(DockFlyout.IsAnyOpen, "Open flyout must remain open while viewing even if cursor leaves.");
            check(!controller.IsHidden, "Dock must stay visible while a widget flyout is actively open.");

            // Toggling ClockButton closed closes the flyout and lets the dock auto-hide
            clockBtn.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, 0, MouseButton.Left) { RoutedEvent = UIElement.PreviewMouseDownEvent });
            clockBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            PumpDispatcher(application.Dispatcher, 100);
            check(!DockFlyout.IsAnyOpen, "Toggling ClockButton with mouse down must close the flyout.");
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden, "Dock must auto-hide after open flyout is closed.");

            // 2. Reveal dock, open flyout again, then close via DockFlyout.CloseAll (simulating outside click/dismiss)
            simulatedCursor = (-9700, -9970);
            controller.OnMouseEnterDock();
            check(!controller.IsHidden, "Hovering dock must reveal it.");
            PumpDispatcher(application.Dispatcher, 400);
            clockBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            PumpDispatcher(application.Dispatcher, 100);
            check(DockFlyout.IsAnyOpen, "Clicking clock button must reopen calendar flyout.");

            // Clicking outside (CloseAll) must dismiss flyout and allow auto-hide
            DockFlyout.CloseAll();
            PumpDispatcher(application.Dispatcher, 400);
            check(!DockFlyout.IsAnyOpen, "Clicking outside must close the flyout.");

            // 3. Test toggle directly via Click while open
            clockBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            PumpDispatcher(application.Dispatcher, 100);
            check(DockFlyout.IsAnyOpen, "Clicking clock button must reopen calendar flyout for toggle test.");
            clockBtn.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            PumpDispatcher(application.Dispatcher, 100);
            check(!DockFlyout.IsAnyOpen, "Clicking clock button again directly must toggle calendar flyout closed.");

            simulatedCursor = (0, 0);
            controller.EvaluateState();
            PumpUntil(application.Dispatcher, () => controller.IsHidden, waitBudgetMs);
            check(controller.IsHidden, "Dock must auto-hide after flyout was dismissed.");
        }
        finally
        {
            DockFlyout.CloseAll();
            controller?.Dispose();
            clock.Deactivate();
            host.Close();
        }
    }

    private static void PumpDispatcher(Dispatcher dispatcher, int milliseconds)
    {
        var frame = new DispatcherFrame();
        var timer = new DispatcherTimer(TimeSpan.FromMilliseconds(milliseconds), DispatcherPriority.Background,
            (_, _) => frame.Continue = false, dispatcher);
        timer.Start();
        Dispatcher.PushFrame(frame);
        timer.Stop();
    }

    private static void PumpUntil(Dispatcher dispatcher, Func<bool> condition, int timeoutMs)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        while (!condition() && Environment.TickCount64 < deadline)
        {
            PumpDispatcher(dispatcher, 35);
        }
    }

    private static T? FindFirstVisual<T>(DependencyObject root) where T : DependencyObject
    {
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T match) return match;
            var nested = FindFirstVisual<T>(child);
            if (nested is not null) return nested;
        }
        return null;
    }
}
