using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Widgets;

internal static class BluetoothBatteryChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckWidgetProperties(Check);
        CheckDisplayWithDevices(Check);
        CheckCompactMode(Check);
        CheckHoverFlyout(Check);

        return checks;
    }

    private static void CheckWidgetProperties(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget
        {
            BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>([])
        };
        check(BluetoothBatteryWidget.Id == "BluetoothBattery", "Id must be 'BluetoothBattery'.");
        check(widget.SupportsCompactMode, "BluetoothBatteryWidget must support compact mode.");
        check(widget.ShrinkPriority == 32, "Shrink priority must be 32.");
        check(widget.CompactWidthEstimate == 46, "Compact width estimate must be 46 DIP.");
        check(widget.FullWidthEstimate == 78, "Full width estimate must be 78 DIP.");

        check(widget.Visibility == Visibility.Collapsed, "Widget must start collapsed before activation.");
        widget.Activate();
        check(widget.Visibility == Visibility.Visible, "Widget must become visible on activation.");
        widget.Deactivate();
        check(widget.Visibility == Visibility.Collapsed, "Widget must collapse on deactivation.");
    }

    private static void CheckDisplayWithDevices(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        widget.BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>(
        [
            new("ROG HARPE ACE AIM LAB", "ble1", true, 53),
            new("Xbox Wireless Controller", "ble2", false, 8)
        ]);

        widget.Activate();
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        check(widget.Content is Button, "Widget content must be a Button.");
        var button = (Button)widget.Content;
        check(System.Windows.Automation.AutomationProperties.GetHelpText(button).Contains("ROG HARPE ACE AIM LAB: 53%"),
            "The card's summary must contain device name and battery percentage.");
        check(button.ToolTip is null, "The hover card replaces the card's tooltip.");

        // Test low battery state
        widget.BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>(
        [
            new("Xbox Controller Low", "ble3", true, 12)
        ]);
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        check(System.Windows.Automation.AutomationProperties.GetHelpText(button).Contains("12%"),
            "Low battery device must be reflected in display.");
    }

    private static void CheckCompactMode(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        widget.SetCompactState(true);
        check(widget.IsCompact, "Widget must enter compact mode.");
        widget.SetCompactState(false);
        check(!widget.IsCompact, "Widget must exit compact mode.");
    }

    private static void CheckHoverFlyout(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        var clicked = false;
        widget.OpenSettingsAction = () => clicked = true;

        var flyout = widget.BuildHoverFlyoutContent();
        check(flyout is StackPanel, "Hover flyout must return a StackPanel.");

        if (widget.Content is Button button)
        {
            button.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            check(clicked, "Clicking the card button must invoke the settings action.");
        }
    }
}
