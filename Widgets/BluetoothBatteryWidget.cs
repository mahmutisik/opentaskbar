using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using Windows.Devices.Bluetooth;
using Windows.Devices.Bluetooth.GenericAttributeProfile;
using Windows.Devices.Enumeration;

namespace OpenTaskbar.Widgets;

public sealed record BluetoothDeviceBatteryInfo(
    string Name,
    string Id,
    bool IsConnected,
    int BatteryLevel
);

/// <summary>
/// A dock widget that displays the battery levels of connected Bluetooth devices.
/// Queries standard Bluetooth LE GATT Battery Service (0x180F) asynchronously.
/// Conforms to the Widget Height Uniformity Rule (46 DIP).
/// </summary>
public sealed class BluetoothBatteryWidget : DockWidget
{
    public static string Id => "BluetoothBattery";

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 32;
    public override double CompactWidthEstimate => 46;
    public override double FullWidthEstimate => 78;

    private static readonly Guid BatteryServiceUuid = new("0000180F-0000-1000-8000-00805F9B34FB");
    private static readonly Guid BatteryLevelCharUuid = new("00002A19-0000-1000-8000-00805F9B34FB");

    private readonly Button _cardButton;
    private readonly ContentControl _btIcon;
    private readonly TextBlock _batteryText;
    private IReadOnlyList<BluetoothDeviceBatteryInfo> _devices = [];
    private bool _isRefreshing;
    private long _lastRefreshTickMs;
    private const long RefreshIntervalMs = 15000; // Refresh battery every 15s

    internal Func<Task<IReadOnlyList<BluetoothDeviceBatteryInfo>>> BatteryReader { get; set; } = ReadLiveBluetoothBatteriesAsync;
    internal Action OpenSettingsAction { get; set; } = DefaultOpenBluetoothSettings;

    public BluetoothBatteryWidget()
    {
        _btIcon = new ContentControl
        {
            Content = FindResource("IconBluetooth"),
            Width = 14,
            Height = 14,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        _btIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        _btIcon.SetResourceReference(ForegroundProperty, "WidgetAccent");

        _batteryText = new TextBlock
        {
            Text = "—",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(5, 0, 0, 0),
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _batteryText.SetResourceReference(StyleProperty, "WidgetText");

        var stack = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center
        };
        stack.Children.Add(_btIcon);
        stack.Children.Add(_batteryText);

        _cardButton = new Button
        {
            Content = stack,
            Padding = new Thickness(10, 0, 10, 0)
        };
        _cardButton.SetResourceReference(StyleProperty, "WidgetCardButton");
        _cardButton.Click += (_, _) => OpenSettingsAction();

        Content = _cardButton;
        Localize();
    }

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Localize();
        _lastRefreshTickMs = 0;
        RefreshBatteries();
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Localize();

    private void Localize()
    {
        var title = UiText.Get("WidgetBluetoothBattery");
        var desc = UiText.Get("WidgetBluetoothBatteryDescription");
        _cardButton.ToolTip = desc;
        AutomationProperties.SetName(_cardButton, title);
        AutomationProperties.SetName(this, title);
    }

    protected override void OnTick(DateTime now)
    {
        var nowMs = Environment.TickCount64;
        if (nowMs - _lastRefreshTickMs >= RefreshIntervalMs)
        {
            RefreshBatteries();
        }
    }

    protected override void OnCompactChanged(bool compact)
    {
        _batteryText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
    }

    internal async Task RefreshBatteriesAsync()
    {
        try
        {
            var list = await BatteryReader().ConfigureAwait(false);
            if (Dispatcher.CheckAccess())
            {
                _devices = list;
                UpdateDisplay();
            }
            else
            {
                await Dispatcher.InvokeAsync(() =>
                {
                    _devices = list;
                    UpdateDisplay();
                });
            }
        }
        catch
        {
        }
    }

    internal void RefreshBatteries()
    {
        if (_isRefreshing) return;
        _isRefreshing = true;
        _lastRefreshTickMs = Environment.TickCount64;

        _ = Task.Run(async () =>
        {
            try
            {
                await RefreshBatteriesAsync().ConfigureAwait(false);
            }
            finally
            {
                _isRefreshing = false;
            }
        });
    }

    internal void UpdateDisplay()
    {
        // Select primary connected device with battery, or lowest battery among connected
        var connected = _devices.Where(d => d.IsConnected && d.BatteryLevel >= 0).ToList();
        if (connected.Count > 0)
        {
            var lowest = connected.OrderBy(d => d.BatteryLevel).First();
            _batteryText.Text = $"{lowest.BatteryLevel}%";
            _btIcon.SetResourceReference(ForegroundProperty, "WidgetAccent");

            if (lowest.BatteryLevel <= 20)
            {
                _batteryText.SetResourceReference(TextBlock.ForegroundProperty, "WidgetWarning");
            }
            else
            {
                _batteryText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            }

            var tooltip = string.Join("\n", connected.Select(d => $"{d.Name}: {d.BatteryLevel}%"));
            _cardButton.ToolTip = tooltip;
            AutomationProperties.SetName(_cardButton, $"{UiText.Get("WidgetBluetoothBattery")}: {lowest.Name} {lowest.BatteryLevel}%");
        }
        else
        {
            var anyConnected = _devices.Any(d => d.IsConnected);
            _batteryText.Text = anyConnected ? "—" : UiText.Get("BluetoothNoDevicesAbbr");
            _btIcon.SetResourceReference(ForegroundProperty, anyConnected ? "TextPrimary" : "TextSecondary");
            _batteryText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            _cardButton.ToolTip = UiText.Get("WidgetBluetoothBatteryDescription");
            AutomationProperties.SetName(_cardButton, UiText.Get("WidgetBluetoothBattery"));
        }
    }

    public override UIElement? BuildFullHoverFlyoutContent() => BuildHoverFlyoutContent();

    public override UIElement? BuildHoverFlyoutContent()
    {
        var panel = new StackPanel { Width = 240 };
        panel.Children.Add(DockFlyout.Heading(UiText.Get("WidgetBluetoothBattery")));

        if (_devices.Count == 0)
        {
            panel.Children.Add(DockFlyout.Caption(UiText.Get("BluetoothNoDevices")));
            return panel;
        }

        var listPanel = new StackPanel { Margin = new Thickness(0, 6, 0, 4) };
        foreach (var dev in _devices)
        {
            var itemGrid = new Grid { Margin = new Thickness(0, 3, 0, 3) };
            itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var nameStack = new StackPanel { Orientation = Orientation.Vertical };
            nameStack.Children.Add(new TextBlock
            {
                Text = dev.Name,
                FontSize = 11.5,
                FontWeight = FontWeights.SemiBold,
                Foreground = (Brush)FindResource("TextPrimary"),
                TextTrimming = TextTrimming.CharacterEllipsis
            });

            var statusText = dev.IsConnected ? UiText.Get("BluetoothConnected") : UiText.Get("BluetoothDisconnected");
            nameStack.Children.Add(new TextBlock
            {
                Text = statusText,
                FontSize = 10,
                Foreground = dev.IsConnected ? (Brush)FindResource("WidgetAccent") : (Brush)FindResource("TextSecondary")
            });

            var batteryText = new TextBlock
            {
                Text = dev.BatteryLevel >= 0 ? $"{dev.BatteryLevel}%" : "—",
                FontSize = 12,
                FontWeight = FontWeights.Bold,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(8, 0, 0, 0),
                Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
            };

            if (dev.BatteryLevel >= 0)
            {
                batteryText.Foreground = dev.BatteryLevel <= 20
                    ? (Brush)FindResource("WidgetWarning")
                    : (Brush)FindResource("TextPrimary");
            }
            else
            {
                batteryText.Foreground = (Brush)FindResource("TextSecondary");
            }

            Grid.SetColumn(nameStack, 0);
            Grid.SetColumn(batteryText, 1);
            itemGrid.Children.Add(nameStack);
            itemGrid.Children.Add(batteryText);
            listPanel.Children.Add(itemGrid);
        }

        panel.Children.Add(listPanel);
        return panel;
    }

    private static async Task<IReadOnlyList<BluetoothDeviceBatteryInfo>> ReadLiveBluetoothBatteriesAsync()
    {
        var result = new List<BluetoothDeviceBatteryInfo>();

        try
        {
            var selector = BluetoothLEDevice.GetDeviceSelectorFromPairingState(true);
            var requestedProperties = new[] { "System.Devices.Aep.IsConnected" };
            var devices = await DeviceInformation.FindAllAsync(selector, requestedProperties).AsTask().ConfigureAwait(false);

            foreach (var devInfo in devices)
            {
                var name = devInfo.Name;
                var isConnected = devInfo.Properties.TryGetValue("System.Devices.Aep.IsConnected", out var isConnObj) && isConnObj is true;
                var batteryLevel = -1;

                if (isConnected)
                {
                    try
                    {
                        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(2));
                        using var bleDevice = await BluetoothLEDevice.FromIdAsync(devInfo.Id).AsTask(cts.Token).ConfigureAwait(false);
                        if (bleDevice != null)
                        {
                            var gattServices = await bleDevice.GetGattServicesForUuidAsync(BatteryServiceUuid).AsTask(cts.Token).ConfigureAwait(false);
                            if (gattServices.Status == GattCommunicationStatus.Success && gattServices.Services.Count > 0)
                            {
                                foreach (var service in gattServices.Services)
                                {
                                    var chars = await service.GetCharacteristicsForUuidAsync(BatteryLevelCharUuid).AsTask(cts.Token).ConfigureAwait(false);
                                    foreach (var ch in chars.Characteristics)
                                    {
                                        var val = await ch.ReadValueAsync().AsTask(cts.Token).ConfigureAwait(false);
                                        if (val.Status == GattCommunicationStatus.Success && val.Value.Length > 0)
                                        {
                                            var reader = Windows.Storage.Streams.DataReader.FromBuffer(val.Value);
                                            batteryLevel = reader.ReadByte();
                                            break;
                                        }
                                    }
                                    if (batteryLevel >= 0) break;
                                }
                            }
                        }
                    }
                    catch
                    {
                    }
                }

                if (!string.IsNullOrWhiteSpace(name))
                {
                    result.Add(new BluetoothDeviceBatteryInfo(name, devInfo.Id, isConnected, batteryLevel));
                }
            }
        }
        catch
        {
        }

        return result;
    }

    private static void DefaultOpenBluetoothSettings()
    {
        DockFlyout.CloseAll();
        try
        {
            Process.Start(new ProcessStartInfo("ms-settings:bluetooth") { UseShellExecute = true });
        }
        catch
        {
        }
    }
}
