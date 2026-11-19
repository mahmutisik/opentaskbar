using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;
using OpenTaskbar.Widgets;

namespace OpenTaskbar.Tests;

internal static class WidgetScreenshotCapture
{
    private static string OutputDir =>
        Environment.GetEnvironmentVariable("OPENTASKBAR_SCREENSHOT_DIR") is { Length: > 0 } custom
            ? custom
            : Path.Combine(Path.GetTempPath(), "OpenTaskbar", "screenshots");

    public static void CaptureAll(Application app)
    {
        Directory.CreateDirectory(OutputDir);

        foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
        {
            ThemeManager.Apply(theme);
            var suffix = theme == AppTheme.Dark ? "dark" : "light";

            var settings = new DockSettings(
                Language: AppLanguage.Turkish,
                Theme: theme,
                FocusMinutes: 25,
                FocusBreakMinutes: 5,
                FocusSessions: 4,
                StatsMeters: SystemMeters.Cpu | SystemMeters.Memory);

            // 1. Battery Card
            var batteryItem = new DockWidgetItem(DockWidgetService.BatteryId, "Pil") { Value = "85%", IsCharging = true };
            var batteryHost = new ContentControl { Content = batteryItem };
            SavePng(app, batteryHost, 200, 62, $"battery_{suffix}.png");

            // 2. Storage Card
            var storageItem = new DockWidgetItem("Storage", "Depolama") { Value = "142.5 GiB boş" };
            var storageHost = new ContentControl { Content = storageItem };
            SavePng(app, storageHost, 200, 62, $"storage_{suffix}.png");

            // 3. DayProgress Card
            var dayProgressItem = new DockWidgetItem("DayProgress", "Günün İlerlemesi") { Value = "68%" };
            var dayProgressHost = new ContentControl { Content = dayProgressItem };
            SavePng(app, dayProgressHost, 180, 62, $"dayprogress_{suffix}.png");

            // 4. Stopwatch Widget
            var clock = new ParityClock(new DateTimeOffset(2026, 11, 18, 14, 30, 0, TimeSpan.Zero));
            var sw = new StopwatchDockWidget(clock);
            sw.Refresh();
            var swHost = new ContentControl { Content = sw };
            SavePng(app, swHost, 200, 62, $"stopwatch_{suffix}.png");

            // 5. Countdown Widget
            var cd = new CountdownDockWidget(clock) { MinutesText = "15" };
            cd.Refresh();
            var cdHost = new ContentControl { Content = cd };
            SavePng(app, cdHost, 200, 62, $"countdown_{suffix}.png");

            // 6. Clock Widget
            using var clockWidget = new ClockWidget();
            clockWidget.ApplySettings(settings);
            clockWidget.Activate();
            SavePng(app, clockWidget, 180, 62, $"clock_{suffix}.png");

            // 7. Media Widget
            using var mediaWidget = new MediaWidget();
            mediaWidget.ApplySettings(settings);
            mediaWidget.Visibility = Visibility.Visible;
            SavePng(app, mediaWidget, 340, 62, $"media_{suffix}.png");

            // 8. Audio Widget
            using var audioWidget = new AudioWidget();
            audioWidget.ApplySettings(settings);
            audioWidget.Visibility = Visibility.Visible;
            SavePng(app, audioWidget, 210, 62, $"audio_{suffix}.png");

            // 9. QuickSettings Widget
            using var qsWidget = new QuickSettingsWidget();
            qsWidget.ApplySettings(settings);
            qsWidget.Visibility = Visibility.Visible;
            SavePng(app, qsWidget, 210, 62, $"quicksettings_{suffix}.png");

            // 10. SystemStats Widget (CPU & RAM ring meters)
            using var statsWidget = new SystemStatsWidget();
            statsWidget.ApplySettings(settings);
            statsWidget.Visibility = Visibility.Visible;
            SavePng(app, statsWidget, 130, 62, $"systemstats_{suffix}.png");

            // 11. Focus Widget
            using var focusWidget = new FocusWidget();
            focusWidget.ApplySettings(settings);
            focusWidget.Activate();
            focusWidget.Visibility = Visibility.Visible;
            SavePng(app, focusWidget, 200, 62, $"focus_{suffix}.png");

            // 12. Weather Widget
            using var weatherWidget = new WeatherWidget();
            weatherWidget.ApplySettings(settings);
            weatherWidget.Visibility = Visibility.Visible;
            SavePng(app, weatherWidget, 120, 62, $"weather_{suffix}.png");

            // 13. Desktops Widget
            using var desktopsWidget = new DesktopsWidget();
            desktopsWidget.ApplySettings(settings);
            desktopsWidget.Activate();
            desktopsWidget.Visibility = Visibility.Visible;
            SavePng(app, desktopsWidget, 150, 62, $"desktops_{suffix}.png");

            // 14. Tray Widget
            using var trayWidget = new TrayWidget();
            trayWidget.ApplySettings(settings);
            trayWidget.Activate();
            trayWidget.Visibility = Visibility.Visible;
            SavePng(app, trayWidget, 80, 62, $"tray_{suffix}.png");

            // 15. Stack Icon
            var stack = new DockStack(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads"), app.Dispatcher);
            var stackIcon = MainWindow.CreateStackIcon(stack, 128);
            if (stackIcon is not null)
            {
                var img = new Image { Source = stackIcon, Width = 56, Height = 56, Stretch = Stretch.Uniform };
                SavePng(app, img, 80, 80, $"stack_icon_{suffix}.png");
            }
        }

        ThemeManager.Apply(AppTheme.Dark);
    }

    private static void SavePng(Application app, FrameworkElement element, int width, int height, string filename)
    {
        var host = new Border
        {
            Width = width,
            Height = height,
            Background = (Brush)app.FindResource("DockMaterial"),
            Padding = new Thickness(6),
            Child = element
        };
        host.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        host.SetResourceReference(TextBlock.FontFamilyProperty, "InterfaceFont");
        host.Measure(new Size(width, height));
        host.Arrange(new Rect(0, 0, width, height));
        host.UpdateLayout();
        app.Dispatcher.Invoke(() => { }, DispatcherPriority.Render);

        var rtb = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        rtb.Render(host);

        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(rtb));
        using var stream = File.Create(Path.Combine(OutputDir, filename));
        encoder.Save(stream);
        host.Child = null;
    }

    private sealed class ParityClock(DateTimeOffset now) : TimeProvider
    {
        public override DateTimeOffset GetUtcNow() => now;
    }
}
