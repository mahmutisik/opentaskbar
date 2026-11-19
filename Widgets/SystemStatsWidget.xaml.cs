using System.Globalization;
using System.Windows;
using System.Windows.Threading;
using System.Windows.Data;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

public partial class SystemStatsWidget : DockWidget
{
    private const int LowBatteryPercent = 15;
    private const double TemperatureScaleCelsius = 100;
    private const double RingValueEpsilon = 0.005;
    private const long FastSamplePeriodMs = 1800;
    private const long SensorSamplePeriodMs = 3800;

    private struct RingState
    {
        public bool Initialized;
        public int Shown;
        public double Value;
        public string? BrushKey;
        public AppLanguage Language;
    }

    private readonly SystemStats _stats = new();
    private readonly object _sensorGate = new();
    private readonly TwoStepConfirm _confirm;
    private SystemSensors? _sensors;
    private int _sensorGeneration;
    private int _sensorSampling;
    private long _lastFastSampleTickMs;
    private long _lastSensorSampleTickMs;

    private RingState _cpuState;
    private RingState _memoryState;
    private RingState _gpuState;
    private RingState _cpuTempState;
    private RingState _gpuTempState;
    private BatteryStatus _batteryState;
    private bool _batteryInitialized;
    private AppLanguage _batteryLanguage;
    private bool _hasGpu;
    private bool _hasCpuTemp;
    private bool _hasGpuTemp;
    private readonly Dictionary<FrameworkElement, int> _missingSensorSamples = [];

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 30;
    public override double CompactWidthEstimate => 88;
    public override double FullWidthEstimate
    {
        get
        {
            var meters = Settings.StatsMeters;
            var count = 0;
            if (meters.HasFlag(SystemMeters.Battery) && _batteryState.Present) count++;
            if (meters.HasFlag(SystemMeters.Cpu)) count++;
            if (meters.HasFlag(SystemMeters.Memory)) count++;
            if (meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp) count++;
            if (meters.HasFlag(SystemMeters.Gpu) && _hasGpu) count++;
            if (meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp) count++;
            return Math.Max(52, count * 52);
        }
    }

    public SystemStatsWidget()
    {
        InitializeComponent();
        _confirm = new TwoStepConfirm(Dispatcher);
    }

    protected override void OnCompactChanged(bool compact) => SyncMeterVisibility();
    protected override void OnHoverExpandedChanged(bool expanded) => SyncMeterVisibility();

    public override UIElement? BuildHoverFlyoutContent()
    {
        var panel = new System.Windows.Controls.WrapPanel { MaxWidth = 300 };
        var meters = Settings.StatsMeters;

        if (meters.HasFlag(SystemMeters.Battery) && _batteryState.Present)
            panel.Children.Add(MakeStatTile(UiText.Get("Battery"), UiText.Format("Percent", _batteryState.Percent), _batteryState.Percent / 100.0, BatteryButton, BatteryRing));
        if (meters.HasFlag(SystemMeters.Cpu))
            panel.Children.Add(MakeStatTile("CPU", $"{_cpuState.Shown}%", _cpuState.Value, CpuButton, CpuRing));
        if (meters.HasFlag(SystemMeters.Memory))
            panel.Children.Add(MakeStatTile("RAM", $"{_memoryState.Shown}%", _memoryState.Value, MemoryButton, MemoryRing));
        if (meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp)
            panel.Children.Add(MakeStatTile("CPU°", $"{_cpuTempState.Shown}°", _cpuTempState.Value, CpuTemperatureButton, CpuTemperatureRing));
        if (meters.HasFlag(SystemMeters.Gpu) && _hasGpu)
            panel.Children.Add(MakeStatTile("GPU", $"{_gpuState.Shown}%", _gpuState.Value, GpuButton, GpuRing));
        if (meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp)
            panel.Children.Add(MakeStatTile("GPU°", $"{_gpuTempState.Shown}°", _gpuTempState.Value, GpuTemperatureButton, GpuTemperatureRing));

        return panel.Children.Count > 0 ? panel : null;

        System.Windows.Controls.Button MakeStatTile(string caption, string valueText, double normalized, System.Windows.Controls.Button sourceButton, RingMeter sourceRing)
        {
            var ring = new RingMeter
            {
                Width = 42,
                Height = 42,
                StrokeThickness = 3.5,
                FontSize = 11.5,
                Caption = caption,
                Text = valueText,
                Value = normalized
            };
            ring.SetResourceReference(RingMeter.CaptionBrushProperty, "TextSecondary");
            ring.SetResourceReference(RingMeter.TrackBrushProperty, "WidgetTrack");
            ring.SetBinding(RingMeter.ValueProperty, new Binding(nameof(RingMeter.Value)) { Source = sourceRing, Mode = BindingMode.OneWay });
            ring.SetBinding(RingMeter.TextProperty, new Binding(nameof(RingMeter.Text)) { Source = sourceRing, Mode = BindingMode.OneWay });
            ring.SetBinding(RingMeter.ValueBrushProperty, new Binding(nameof(RingMeter.ValueBrush)) { Source = sourceRing, Mode = BindingMode.OneWay });
            ring.SetResourceReference(ForegroundProperty, "TextPrimary");
            var btn = new System.Windows.Controls.Button
            {
                Width = 54,
                Height = 54,
                Padding = new Thickness(3),
                Margin = new Thickness(3),
                Content = ring,
                ToolTip = sourceButton.ToolTip
            };
            btn.SetResourceReference(StyleProperty, "WidgetCardButton");
            btn.Click += TaskManager_Click;
            return btn;
        }
    }

    protected override void OnActivated()
    {
        _stats.SampleCpu();
        // Opening the widget must never wait for a slow native sensor query.
        Interlocked.Increment(ref _sensorGeneration);

        var meters = Settings.StatsMeters;
        _hasGpu = meters.HasFlag(SystemMeters.Gpu);
        _hasCpuTemp = meters.HasFlag(SystemMeters.CpuTemperature);
        _hasGpuTemp = meters.HasFlag(SystemMeters.GpuTemperature);

        ApplyMeterSelection();
        // Show Memory & Battery right away, and schedule the first CPU/sensor sample.
        SampleFastCounters(sampleCpu: false);
        var nowMs = Environment.TickCount64;
        _lastFastSampleTickMs = nowMs - 900;
        _lastSensorSampleTickMs = nowMs - 2900;
        PrimeSensorBaselineIfNeeded();
    }

    protected override void OnDeactivated()
    {
        _confirm.Disarm();
        var generation = Interlocked.Increment(ref _sensorGeneration);
        // Dispose from the worker after any in-flight native query completes.
        _ = Task.Run(() =>
        {
            lock (_sensorGate)
            {
                if (generation != Volatile.Read(ref _sensorGeneration)) return;
                _sensors?.Dispose();
                _sensors = null;
            }
        });
        _lastFastSampleTickMs = 0;
        _lastSensorSampleTickMs = 0;
    }

    protected override void OnDisposed()
    {
        _confirm.Dispose();
        base.OnDisposed();
    }

    protected override void OnSettingsChanged()
    {
        Interlocked.Increment(ref _sensorGeneration);

        var meters = Settings.StatsMeters;
        _hasGpu = meters.HasFlag(SystemMeters.Gpu);
        _hasCpuTemp = meters.HasFlag(SystemMeters.CpuTemperature);
        _hasGpuTemp = meters.HasFlag(SystemMeters.GpuTemperature);

        ApplyMeterSelection();
        if (IsActive)
        {
            _lastFastSampleTickMs = 0;
            _lastSensorSampleTickMs = 0;
            OnTick(DateTime.Now);
        }
    }

    protected override void OnTick(DateTime now)
    {
        // CPU/RAM update every ~2s; heavy PDH/NVML sensors update every ~4s.
        var tickMs = Environment.TickCount64;
        if (_lastFastSampleTickMs == 0 || tickMs - _lastFastSampleTickMs >= FastSamplePeriodMs)
        {
            _lastFastSampleTickMs = tickMs;
            SampleFastCounters(sampleCpu: true);
        }

        if (_lastSensorSampleTickMs == 0 || tickMs - _lastSensorSampleTickMs >= SensorSamplePeriodMs)
        {
            _lastSensorSampleTickMs = tickMs;
            QueueSensorSample(Settings.StatsMeters);
        }
    }

    private void SampleFastCounters(bool sampleCpu)
    {
        var meters = Settings.StatsMeters;
        if (sampleCpu && meters.HasFlag(SystemMeters.Cpu) && _stats.SampleCpu() is { } cpu)
            SetUsage(CpuRing, CpuButton, ref _cpuState, cpu, "Cpu");
        if (meters.HasFlag(SystemMeters.Memory) && SystemStats.SampleMemory() is { } memory)
            SetUsage(MemoryRing, MemoryButton, ref _memoryState, memory, "Memory");
        UpdateBattery(meters.HasFlag(SystemMeters.Battery) ? SystemStats.SampleBattery() : new BatteryStatus(false, 0, false, false));
    }

    private void PrimeSensorBaselineIfNeeded()
    {
        var meters = Settings.StatsMeters;
        if ((meters & (SystemMeters.Gpu | SystemMeters.CpuTemperature | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);
        _ = Task.Run(async () =>
        {
            double? gpu = null;
            double? cpuTemp = null;
            double? gpuTemp = null;
            try
            {
                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    _sensors ??= new SystemSensors(meters);
                    _sensors.Configure(meters);
                    if (meters.HasFlag(SystemMeters.Gpu)) _sensors.SampleGpuUsage();
                }

                // Short delay so D3DKMT counters can compute a valid initial delta without waiting 4s
                await Task.Delay(350).ConfigureAwait(false);

                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    if (meters.HasFlag(SystemMeters.Gpu)) gpu = _sensors?.SampleGpuUsage();
                    if (meters.HasFlag(SystemMeters.CpuTemperature)) cpuTemp = _sensors?.SampleCpuTemperature();
                    if (meters.HasFlag(SystemMeters.GpuTemperature)) gpuTemp = _sensors?.SampleGpuTemperature();
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Sensor baseline/initial sample failed: {ex.Message}");
                return;
            }
            finally
            {
                Interlocked.Exchange(ref _sensorSampling, 0);
            }

            _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
            {
                if (!IsActive || generation != Volatile.Read(ref _sensorGeneration)) return;
                var currentMeters = Settings.StatsMeters;
                UpdateSensor(GpuButton, currentMeters.HasFlag(SystemMeters.Gpu) ? gpu : null,
                    usage => SetUsage(GpuRing, GpuButton, ref _gpuState, usage, "Gpu"));
                UpdateSensor(CpuTemperatureButton, currentMeters.HasFlag(SystemMeters.CpuTemperature) ? cpuTemp : null,
                    celsius => SetTemperature(CpuTemperatureRing, CpuTemperatureButton, ref _cpuTempState, celsius, "CpuTemperature"));
                UpdateSensor(GpuTemperatureButton, currentMeters.HasFlag(SystemMeters.GpuTemperature) ? gpuTemp : null,
                    celsius => SetTemperature(GpuTemperatureRing, GpuTemperatureButton, ref _gpuTempState, celsius, "GpuTemperature"));
            });
        });
    }

    private void QueueSensorSample(SystemMeters meters)
    {
        if ((meters & (SystemMeters.Gpu | SystemMeters.CpuTemperature | SystemMeters.GpuTemperature)) == 0) return;
        if (Interlocked.CompareExchange(ref _sensorSampling, 1, 0) != 0) return;
        var generation = Volatile.Read(ref _sensorGeneration);

        _ = Task.Run(() =>
        {
            double? gpu = null;
            double? cpuTemp = null;
            double? gpuTemp = null;
            try
            {
                lock (_sensorGate)
                {
                    if (generation != Volatile.Read(ref _sensorGeneration)) return;
                    _sensors ??= new SystemSensors(meters);
                    _sensors.Configure(meters);
                    if (meters.HasFlag(SystemMeters.Gpu)) gpu = _sensors.SampleGpuUsage();
                    if (meters.HasFlag(SystemMeters.CpuTemperature)) cpuTemp = _sensors.SampleCpuTemperature();
                    if (meters.HasFlag(SystemMeters.GpuTemperature)) gpuTemp = _sensors.SampleGpuTemperature();
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Sensor sampling failed: {ex.Message}");
                return;
            }
            finally
            {
                Interlocked.Exchange(ref _sensorSampling, 0);
            }

            _ = Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
            {
                if (!IsActive || generation != Volatile.Read(ref _sensorGeneration)) return;
                var currentMeters = Settings.StatsMeters;
                UpdateSensor(GpuButton, currentMeters.HasFlag(SystemMeters.Gpu) ? gpu : null,
                    usage => SetUsage(GpuRing, GpuButton, ref _gpuState, usage, "Gpu"));
                UpdateSensor(CpuTemperatureButton, currentMeters.HasFlag(SystemMeters.CpuTemperature) ? cpuTemp : null,
                    celsius => SetTemperature(CpuTemperatureRing, CpuTemperatureButton, ref _cpuTempState, celsius, "CpuTemperature"));
                UpdateSensor(GpuTemperatureButton, currentMeters.HasFlag(SystemMeters.GpuTemperature) ? gpuTemp : null,
                    celsius => SetTemperature(GpuTemperatureRing, GpuTemperatureButton, ref _gpuTempState, celsius, "GpuTemperature"));
            });
        });
    }

    private void ApplyMeterSelection() => SyncMeterVisibility();

    private void SyncMeterVisibility()
    {
        var compact = IsCompact && !IsHoverExpanded;
        var meters = Settings.StatsMeters;
        CompactSummaryButton.Visibility = compact ? Visibility.Visible : Visibility.Collapsed;
        BatteryButton.Visibility = !compact && meters.HasFlag(SystemMeters.Battery) && _batteryState.Present ? Visibility.Visible : Visibility.Collapsed;
        CpuButton.Visibility = !compact && meters.HasFlag(SystemMeters.Cpu) ? Visibility.Visible : Visibility.Collapsed;
        MemoryButton.Visibility = !compact && meters.HasFlag(SystemMeters.Memory) ? Visibility.Visible : Visibility.Collapsed;
        GpuButton.Visibility = !compact && meters.HasFlag(SystemMeters.Gpu) && _hasGpu ? Visibility.Visible : Visibility.Collapsed;
        CpuTemperatureButton.Visibility = !compact && meters.HasFlag(SystemMeters.CpuTemperature) && _hasCpuTemp ? Visibility.Visible : Visibility.Collapsed;
        GpuTemperatureButton.Visibility = !compact && meters.HasFlag(SystemMeters.GpuTemperature) && _hasGpuTemp ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateSensor(FrameworkElement button, double? reading, Action<double> render)
    {
        if (reading is null)
        {
            _missingSensorSamples.TryGetValue(button, out var misses);
            misses = Math.Min(misses + 1, 3);
            _missingSensorSamples[button] = misses;
            // A single missed sample should not make the dock jump in width.
            if (misses < 3) return;
        }
        else
        {
            _missingSensorSamples.Remove(button);
        }

        var available = reading is not null;
        if (button == GpuButton) _hasGpu = available;
        else if (button == CpuTemperatureButton) _hasCpuTemp = available;
        else if (button == GpuTemperatureButton) _hasGpuTemp = available;
        if (reading is { } value) render(value);
        SyncMeterVisibility();
    }

    private void SetUsage(RingMeter ring, FrameworkElement owner, ref RingState state, double value, string labelKey)
    {
        var percent = (int)Math.Round(value * 100);
        var lang = Settings.Language;
        var brushKey = value >= 0.9 ? "WidgetFocusAccent" : value >= 0.75 ? "WidgetWarning" : "WidgetAccent";
        if (state.Initialized
            && state.Shown == percent
            && state.BrushKey == brushKey
            && state.Language == lang
            && Math.Abs(value - state.Value) < RingValueEpsilon)
        {
            return;
        }

        if (!state.Initialized || Math.Abs(value - state.Value) >= RingValueEpsilon)
        {
            ring.Value = value;
            state.Value = value;
        }
        if (!state.Initialized || state.Shown != percent)
        {
            ring.Text = percent.ToString(CultureInfo.InvariantCulture);
        }
        if (state.BrushKey != brushKey)
        {
            ring.SetResourceReference(RingMeter.ValueBrushProperty, brushKey);
            state.BrushKey = brushKey;
        }
        if (!state.Initialized || state.Shown != percent || state.Language != lang)
        {
            owner.ToolTip = $"{UiText.Get(labelKey)}: {UiText.Format("Percent", percent)}";
            state.Shown = percent;
            state.Language = lang;
            if (owner == CpuButton) CompactCpuText.Text = $"CPU {percent}%";
            else if (owner == MemoryButton) CompactRamText.Text = $"RAM {percent}%";
            CompactSummaryButton.ToolTip = $"{CompactCpuText.Text} · {CompactRamText.Text}";
        }
        state.Initialized = true;
    }

    private void SetTemperature(RingMeter ring, FrameworkElement owner, ref RingState state, double celsius, string labelKey)
    {
        var rounded = (int)Math.Round(celsius);
        var normalized = celsius / TemperatureScaleCelsius;
        var lang = Settings.Language;
        var brushKey = normalized >= 0.9 ? "WidgetFocusAccent" : normalized >= 0.75 ? "WidgetWarning" : "WidgetAccent";
        if (state.Initialized
            && state.Shown == rounded
            && state.BrushKey == brushKey
            && state.Language == lang
            && Math.Abs(normalized - state.Value) < RingValueEpsilon)
        {
            return;
        }

        if (!state.Initialized || Math.Abs(normalized - state.Value) >= RingValueEpsilon)
        {
            ring.Value = normalized;
            state.Value = normalized;
        }
        if (!state.Initialized || state.Shown != rounded)
        {
            ring.Text = rounded.ToString(CultureInfo.InvariantCulture);
        }
        if (state.BrushKey != brushKey)
        {
            ring.SetResourceReference(RingMeter.ValueBrushProperty, brushKey);
            state.BrushKey = brushKey;
        }
        if (!state.Initialized || state.Shown != rounded || state.Language != lang)
        {
            owner.ToolTip = $"{UiText.Get(labelKey)}: {rounded}°C";
            state.Shown = rounded;
            state.Language = lang;
        }
        state.Initialized = true;
    }

    private void UpdateBattery(BatteryStatus battery)
    {
        var lang = Settings.Language;
        if (_batteryInitialized && _batteryState == battery && _batteryLanguage == lang) return;
        _batteryInitialized = true;
        _batteryState = battery;
        _batteryLanguage = lang;

        SyncMeterVisibility();
        if (!battery.Present) return;
        BatteryRing.Value = Math.Clamp(battery.Percent / 100.0, 0, 1);
        BatteryRing.Text = battery.Percent.ToString(CultureInfo.InvariantCulture);
        BatteryRing.Caption = lang == AppLanguage.Turkish ? "PİL" : "BAT";
        var low = DockWidgetService.IsLowBattery(battery.Percent, battery.PluggedIn);
        BatteryRing.SetResourceReference(RingMeter.ValueBrushProperty, low ? "WidgetFocusAccent" : "WidgetAccent");
        WidgetTint.Apply(BatteryButton, low ? WidgetTint.Red : WidgetTint.Teal);
        ChargingIcon.Visibility = battery.Charging ? Visibility.Visible : Visibility.Collapsed;
        BatteryButton.ToolTip = $"{UiText.Get(battery.Charging ? "BatteryCharging" : "Battery")}: {UiText.Format("Percent", battery.Percent)}";
    }

    private void BatteryButton_Click(object sender, RoutedEventArgs e) => Report(ShellShortcuts.OpenUri("ms-settings:batterysaver"));

    private void TaskManager_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not System.Windows.Controls.Button button)
        {
            Report(ShellShortcuts.OpenTaskManager());
            return;
        }

        var previousToolTip = button.ToolTip;
        if (!_confirm.ConsumeOrArm(
            button,
            onArm: () =>
            {
                button.BorderThickness = new Thickness(2);
                button.SetResourceReference(System.Windows.Controls.Control.BorderBrushProperty, "AccentBrush");
                button.ToolTip = UiText.Get("TaskManagerConfirm");
            },
            onDisarm: () =>
            {
                button.ClearValue(System.Windows.Controls.Control.BorderThicknessProperty);
                button.ClearValue(System.Windows.Controls.Control.BorderBrushProperty);
                button.ToolTip = previousToolTip;
            }))
        {
            return;
        }

        Report(ShellShortcuts.OpenTaskManager());
    }
}
