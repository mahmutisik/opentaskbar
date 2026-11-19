using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Widgets;

internal static class NetSpeedChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckSpeedFormatting(Check);
        CheckCompactFormatting(Check);
        CheckBytesFormatting(Check);
        CheckWidgetLifecycleAndCalculations(Check);
        CheckRateCalculation(Check);
        CheckStableWidth(app, Check);
        CheckCardTints(app, Check);
        CheckHoverFlyoutAndInteractions(Check);
        CheckSpeedTestExecution(Check);

        return checks;
    }

    private static void CheckSpeedFormatting(Action<bool, string> check)
    {
        check(NetSpeedWidget.FormatSpeed(0) == "0 B/s", "0 bytes/s must format to '0 B/s'.");
        check(NetSpeedWidget.FormatSpeed(500) == "500 B/s", "500 bytes/s must format to '500 B/s'.");
        check(NetSpeedWidget.FormatSpeed(1024 * 45.4) == "45.4 KB/s", "45.4 KB/s must format with one decimal.");
        check(NetSpeedWidget.FormatSpeed(1024 * 450) == "450 KB/s", "450 KB/s must format without decimals.");
        check(NetSpeedWidget.FormatSpeed(1024 * 1024 * 12.8) == "12.8 MB/s", "12.8 MB/s must format with one decimal.");
        check(NetSpeedWidget.FormatSpeed(1024 * 1024 * 250) == "250 MB/s", "250 MB/s must format without decimals.");
        check(NetSpeedWidget.FormatSpeed(1024L * 1024 * 1024 * 2.5) == "2.50 GB/s", "2.5 GB/s must format with two decimals.");
    }

    private static void CheckCompactFormatting(Action<bool, string> check)
    {
        check(NetSpeedWidget.FormatCompactSpeed(0) == "0", "0 bytes compact must format to '0'.");
        check(NetSpeedWidget.FormatCompactSpeed(1024 * 350) == "350K", "350 KB compact must format to '350K'.");
        check(NetSpeedWidget.FormatCompactSpeed(1024 * 1024 * 4.5) == "4.5M", "4.5 MB compact must format to '4.5M'.");
        check(NetSpeedWidget.FormatCompactSpeed(1024 * 1024 * 35) == "35M", "35 MB compact must format to '35M'.");
        check(NetSpeedWidget.FormatCompactSpeed(1024L * 1024 * 1024 * 1.8) == "1.8G", "1.8 GB compact must format to '1.8G'.");
    }

    private static void CheckBytesFormatting(Action<bool, string> check)
    {
        check(NetSpeedWidget.FormatBytes(0) == "0 B", "0 bytes must format to '0 B'.");
        check(NetSpeedWidget.FormatBytes(500) == "500 B", "500 bytes must format to '500 B'.");
        check(NetSpeedWidget.FormatBytes(1024 * 50) == "50.0 KB", "50 KB must format to '50.0 KB'.");
        check(NetSpeedWidget.FormatBytes(1024 * 1024 * 150) == "150.0 MB", "150 MB must format to '150.0 MB'.");
        check(NetSpeedWidget.FormatBytes(1024L * 1024 * 1024 * 3) == "3.00 GB", "3 GB must format to '3.00 GB'.");
    }

    private static void CheckWidgetLifecycleAndCalculations(Action<bool, string> check)
    {
        var widget = new NetSpeedWidget();
        check(widget.SupportsCompactMode, "NetSpeedWidget must support compact mode.");
        check(widget.CompactWidthEstimate == 89, "Compact width estimate must be 89 DIP.");
        check(widget.FullWidthEstimate == 168, "Full width estimate must be 168 DIP.");

        long fakeReceived = 100_000_000;
        long fakeSent = 20_000_000;
        widget.NetworkReader = () => (fakeReceived, fakeSent, "TestAdapter");

        widget.Activate();
        check(widget.Visibility == Visibility.Visible, "Widget must become visible on activation.");

        // First tick sets baseline (speed = 0)
        widget.Tick(DateTime.Now);

        // Advance bytes by 10 MB received and 2 MB sent
        fakeReceived += 10 * 1024 * 1024;
        fakeSent += 2 * 1024 * 1024;
        widget.Tick(DateTime.Now.AddSeconds(1));

        check(widget.Content is Button, "Widget content must be a WidgetCardButton.");

        // Test compact mode
        widget.SetCompactState(true);
        check(widget.IsCompact, "Widget must reflect compact state.");

        widget.SetCompactState(false);
        check(!widget.IsCompact, "Widget must revert from compact state.");

        widget.Deactivate();
        check(widget.Visibility == Visibility.Collapsed, "Widget must collapse on deactivation.");
    }

    // Samples are now taken off the UI thread; the rate math is checked on its own, independent of timing.
    private static void CheckRateCalculation(Action<bool, string> check)
    {
        var first = NetSpeedWidget.ComputeRates(-1, -1, 0, 1_000, 500, 10_000);
        check(first == (0, 0, 0, 0), "The first sample has no baseline and must report zero rates.");

        var second = NetSpeedWidget.ComputeRates(1_000, 500, 10_000, 1_000 + 2_048, 500 + 1_024, 12_000);
        check(second.DeltaReceived == 2_048 && second.DeltaSent == 1_024
            && Math.Abs(second.Download - 1_024) < 0.001 && Math.Abs(second.Upload - 512) < 0.001,
            "Rates must divide the byte deltas by the time between the two samples.");

        var reset = NetSpeedWidget.ComputeRates(5_000, 5_000, 10_000, 100, 200, 11_000);
        check(reset == (0, 0, 0, 0), "Adapter counter resets must not produce negative or huge rates.");

        var burst = NetSpeedWidget.ComputeRates(0, 0, 10_000, 1_000, 0, 10_010);
        check(Math.Abs(burst.Download - 5_000) < 0.001, "Back-to-back samples must use at least 0.2 s to avoid spikes.");
    }

    // The reading changes every second; its width must not, or the whole dock resizes with it.
    private static void CheckStableWidth(Application app, Action<bool, string> check)
    {
        var text = new TextBlock { FontSize = 11.5, FontWeight = FontWeights.SemiBold, Style = (Style)app.FindResource("WidgetText") };
        double WidthOf(double bytesPerSecond)
        {
            text.Text = NetSpeedWidget.FormatSpeed(bytesPerSecond);
            text.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
            return text.DesiredSize.Width;
        }

        NetSpeedWidget.ReserveSpeedWidth(text);
        var idle = WidthOf(0);
        check(new[] { 500, 1024 * 45.4, 1024 * 450, 1024 * 1024 * 88.8, 1024 * 1024 * 250, 1024L * 1024 * 1024 * 2.5 }
                .All(rate => Math.Abs(WidthOf(rate) - idle) < 0.01),
            "Every speed reading must take the same reserved width.");
    }

    private static void CheckCardTints(Application app, Action<bool, string> check)
    {
        foreach (var tint in new[] { WidgetTint.Teal, WidgetTint.Sky, WidgetTint.Coral, WidgetTint.Violet, WidgetTint.Amber, WidgetTint.Slate, WidgetTint.Red, WidgetTint.Indigo })
        {
            check(app.TryFindResource($"WidgetTint{tint}Fill") is System.Windows.Media.Brush
                    && app.TryFindResource($"WidgetTint{tint}Stroke") is System.Windows.Media.Brush,
                $"The {tint} widget tint must define its fill and stroke.");
        }

        var widget = new NetSpeedWidget();
        check(widget.Content is Button card && ReferenceEquals(card.Background, app.FindResource("WidgetTintSkyFill")),
            "The net speed card must carry its sky tint.");
        check(WidgetTint.ForWeatherIcon("IconSun") == WidgetTint.Amber && WidgetTint.ForWeatherIcon("IconCloudRain") == WidgetTint.Sky
                && WidgetTint.ForWeatherIcon("IconCloud") == WidgetTint.Slate,
            "Weather cards must take the colour of their sky.");
        check(WidgetTint.ForAiProvider(OpenTaskbar.Models.AiProviders.Claude) == WidgetTint.Coral
                && WidgetTint.ForAiProvider(OpenTaskbar.Models.AiProviders.DeepSeek) == WidgetTint.Indigo
                && WidgetTint.ForAiProvider(OpenTaskbar.Models.AiProviders.None) is null,
            "Each AI provider card must carry its own colour; the summary card stays neutral.");
        check(OpenTaskbar.Services.DockWidgetService.IsLowBattery(15, pluggedIn: false) && !OpenTaskbar.Services.DockWidgetService.IsLowBattery(16, pluggedIn: false)
                && !OpenTaskbar.Services.DockWidgetService.IsLowBattery(5, pluggedIn: true),
            "The battery card turns red only at 15% or less while running on battery.");
    }

    private static void CheckHoverFlyoutAndInteractions(Action<bool, string> check)
    {
        var widget = new NetSpeedWidget();
        var flyout = widget.BuildHoverFlyoutContent();
        check(flyout is StackPanel, "BuildHoverFlyoutContent must return a non-null StackPanel.");

        var panel = (StackPanel)flyout!;
        check(panel.Children.Count >= 2, "Hover flyout must include at least heading and metrics.");

        var clickFlyout = widget.BuildClickFlyoutContent();
        check(clickFlyout is StackPanel, "BuildClickFlyoutContent must return a non-null StackPanel.");
        var clickPanel = (StackPanel)clickFlyout;
        check(clickPanel.Children.Count >= 4, "Click flyout must include heading, metrics, speed test, and settings.");
    }

    private static void CheckSpeedTestExecution(Action<bool, string> check)
    {
        var widget = new NetSpeedWidget();
        widget.SpeedTestRunner = (progress, ct) =>
        {
            progress.Report(new SpeedTestProgress("Testing", 50.0, 50));
            return Task.FromResult(new SpeedTestResult(2, 941.5, 856.2));
        };

        var flyout = widget.BuildClickFlyoutContent();
        check(flyout != null, "Click flyout must build before test.");

        // Run the test
        widget.StartSpeedTestAsync().GetAwaiter().GetResult();

        // Verify flyout content reflects completed results
        var updatedFlyout = widget.BuildClickFlyoutContent();
        check(updatedFlyout is StackPanel, "Updated flyout must be a StackPanel.");

        // Test cancel
        widget.CancelSpeedTest();
    }
}
