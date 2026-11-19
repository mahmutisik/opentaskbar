using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Net.Http;
using System.Net.NetworkInformation;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

public enum SpeedTestStatus
{
    Idle,
    Running,
    Completed,
    Failed
}

public sealed record SpeedTestProgress(string StatusText, double CurrentMbps, double Percent);
public sealed record SpeedTestResult(int PingMs, double DownloadMbps, double UploadMbps);

/// <summary>
/// A dock widget that displays real-time internet download and upload speeds side-by-side.
/// Clicking it opens an interactive flyout allowing the user to run a multi-stream, privacy-first speed test.
/// Conforms to the Widget Height Uniformity Rule (46 DIP).
/// </summary>
public sealed class NetSpeedWidget : DockWidget
{
    public static string Id => "NetSpeed";

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 30;
    // Padding, arrow and the reserved reading width (see ReserveSpeedWidth).
    public override double CompactWidthEstimate => 89;
    public override double FullWidthEstimate => 168;

    private readonly Button _cardButton;
    private readonly ContentControl _downIcon;
    private readonly TextBlock _downloadText;
    private readonly ContentControl _upIcon;
    private readonly TextBlock _uploadText;
    private readonly StackPanel _uploadGroup;

    private readonly DockFlyout _flyout = new();
    private StackPanel? _flyoutPanel;
    private StackPanel? _speedTestContainer;

    private long _lastReceivedBytes = -1;
    private long _lastSentBytes = -1;
    private long _lastTickMs;
    private bool _sampleInFlight;
    private int _sampleGeneration;
    private double _downloadBytesPerSec;
    private double _uploadBytesPerSec;
    private long _sessionDownloadedBytes;
    private long _sessionUploadedBytes;
    private string _primaryInterfaceName = string.Empty;

    // Speed test state
    private SpeedTestStatus _testStatus = SpeedTestStatus.Idle;
    private SpeedTestResult? _lastTestResult;
    private string _testStatusMessage = string.Empty;
    private double _testLiveMbps;
    private double _testProgressPercent;
    private CancellationTokenSource? _testCts;

    private static readonly HttpClient SharedHttpClient = new(new SocketsHttpHandler
    {
        PooledConnectionLifetime = TimeSpan.FromMinutes(2),
        MaxConnectionsPerServer = 16
    })
    {
        Timeout = TimeSpan.FromSeconds(15)
    };

    private static readonly byte[] UploadPayloadChunk = new byte[2 * 1024 * 1024];

    internal Func<(long Received, long Sent, string PrimaryInterfaceName)> NetworkReader { get; set; } = ReadNetworkBytes;
    internal Func<IProgress<SpeedTestProgress>, CancellationToken, Task<SpeedTestResult>> SpeedTestRunner { get; set; } = RunMultiStreamSpeedTestAsync;
    internal Action OpenSettingsAction { get; set; } = DefaultOpenSettings;

    public NetSpeedWidget()
    {
        _downIcon = new ContentControl
        {
            Content = FindResource("IconArrowDown"),
            Width = 12,
            Height = 12,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        _downIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        _downIcon.SetResourceReference(ForegroundProperty, "WidgetAccent");

        _downloadText = new TextBlock
        {
            Text = "0 B/s",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 0, 0),
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _downloadText.SetResourceReference(StyleProperty, "WidgetText");

        var downGroup = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center
        };
        downGroup.Children.Add(_downIcon);
        downGroup.Children.Add(_downloadText);

        _upIcon = new ContentControl
        {
            Content = FindResource("IconArrowUp"),
            Width = 12,
            Height = 12,
            VerticalAlignment = VerticalAlignment.Center,
            IsHitTestVisible = false
        };
        _upIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        _upIcon.SetResourceReference(ForegroundProperty, "TextSecondary");

        _uploadText = new TextBlock
        {
            Text = "0 B/s",
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(4, 0, 0, 0),
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        };
        _uploadText.SetResourceReference(StyleProperty, "WidgetText");

        _uploadGroup = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(10, 0, 0, 0)
        };
        _uploadGroup.Children.Add(_upIcon);
        _uploadGroup.Children.Add(_uploadText);

        var contentRow = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center
        };
        contentRow.Children.Add(downGroup);
        contentRow.Children.Add(_uploadGroup);

        _cardButton = new Button
        {
            Content = contentRow,
            Padding = new Thickness(10, 0, 10, 0)
        };
        _cardButton.SetResourceReference(StyleProperty, "WidgetCardButton");
        WidgetTint.Apply(_cardButton, WidgetTint.Sky);
        _cardButton.Click += CardButton_Click;
        _downloadText.Loaded += (_, _) => ReserveSpeedWidth(_downloadText);
        _uploadText.Loaded += (_, _) => ReserveSpeedWidth(_uploadText);

        Content = _cardButton;
        Localize();
    }

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        Localize();
        // A sample from a previous activation would otherwise skip the first tick and then land with reset baselines.
        _sampleGeneration++;
        _sampleInFlight = false;
        _lastReceivedBytes = -1;
        _lastSentBytes = -1;
        _lastTickMs = 0;
        OnTick(DateTime.Now);
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _sampleGeneration++;
        _sampleInFlight = false;
        CancelSpeedTest();
        _flyout.Close();
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e) => Localize();

    private void Localize()
    {
        var description = UiText.Get("WidgetNetSpeedDescription");
        var heading = UiText.Get("WidgetNetSpeed");
        _cardButton.ToolTip = description;
        AutomationProperties.SetName(_cardButton, heading);
        AutomationProperties.SetName(this, heading);
    }

    protected override void OnTick(DateTime now)
    {
        // Enumerating adapters and their statistics takes ~40 ms; doing it every second on the UI thread made the
        // whole dock stutter. Sample off the UI thread and apply the result when it arrives; skip a tick while the
        // previous sample is still running.
        if (_sampleInFlight) return;
        _sampleInFlight = true;
        var generation = _sampleGeneration;
        var reader = NetworkReader;
        _ = Task.Run(() =>
        {
            var sample = reader();
            return (Sample: sample, SampledAtMs: Environment.TickCount64);
        }).ContinueWith(task => Dispatcher.BeginInvoke(() =>
        {
            if (generation != _sampleGeneration) return;
            _sampleInFlight = false;
            if (!IsActive || !task.IsCompletedSuccessfully) return;
            ApplySample(task.Result.Sample, task.Result.SampledAtMs);
        }), TaskScheduler.Default);
    }

    private void ApplySample((long Received, long Sent, string PrimaryInterfaceName) sample, long sampledAtMs)
    {
        _primaryInterfaceName = sample.PrimaryInterfaceName;
        var rates = ComputeRates(_lastReceivedBytes, _lastSentBytes, _lastTickMs, sample.Received, sample.Sent, sampledAtMs);
        _downloadBytesPerSec = rates.Download;
        _uploadBytesPerSec = rates.Upload;
        _sessionDownloadedBytes += rates.DeltaReceived;
        _sessionUploadedBytes += rates.DeltaSent;

        _lastReceivedBytes = sample.Received;
        _lastSentBytes = sample.Sent;
        _lastTickMs = sampledAtMs;

        UpdateLabels();
    }

    /// <summary>Byte rates between two samples; the first sample (no baseline) and counter resets yield zero.</summary>
    internal static (double Download, double Upload, long DeltaReceived, long DeltaSent) ComputeRates(
        long lastReceived, long lastSent, long lastMs, long received, long sent, long nowMs)
    {
        if (lastReceived < 0 || lastSent < 0 || lastMs <= 0) return (0, 0, 0, 0);
        var elapsedSeconds = Math.Max(0.2, (nowMs - lastMs) / 1000.0);
        var deltaReceived = received >= lastReceived ? received - lastReceived : 0;
        var deltaSent = sent >= lastSent ? sent - lastSent : 0;
        return (deltaReceived / elapsedSeconds, deltaSent / elapsedSeconds, deltaReceived, deltaSent);
    }

    /// <summary>Readings of every unit at their widest; FormatSpeed shows at most three digits.</summary>
    private static readonly string[] WidestSpeedReadings = ["888 B/s", "88.8 KB/s", "888 KB/s", "88.8 MB/s", "888 MB/s", "8.88 GB/s"];

    /// <summary>
    /// Reserves the width of the widest reading, so the card, and the dock around it, keeps one size while the
    /// speed changes every second.
    /// </summary>
    internal static void ReserveSpeedWidth(TextBlock text)
    {
        var typeface = new Typeface(text.FontFamily, text.FontStyle, text.FontWeight, text.FontStretch);
        var pixelsPerDip = VisualTreeHelper.GetDpi(text).PixelsPerDip;
        text.MinWidth = Math.Ceiling(WidestSpeedReadings.Max(reading => new FormattedText(reading, CultureInfo.CurrentUICulture,
            FlowDirection.LeftToRight, typeface, text.FontSize, Brushes.Black, pixelsPerDip).WidthIncludingTrailingWhitespace));
    }

    protected override void OnCompactChanged(bool compact)
    {
        _uploadGroup.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;
        UpdateLabels();
    }

    private void UpdateLabels()
    {
        _downloadText.Text = FormatSpeed(_downloadBytesPerSec);
        _uploadText.Text = FormatSpeed(_uploadBytesPerSec);

        var speedStatus = $"↓ {FormatSpeed(_downloadBytesPerSec)} · ↑ {FormatSpeed(_uploadBytesPerSec)}";
        AutomationProperties.SetName(_cardButton, $"{UiText.Get("WidgetNetSpeed")}: {speedStatus}");
    }

    private void CardButton_Click(object sender, RoutedEventArgs e)
    {
        _flyout.Toggle(_cardButton, BuildClickFlyoutContent, focusable: true);
    }

    public override UIElement? BuildHoverFlyoutContent()
    {
        var panel = new StackPanel { Width = 240 };
        panel.Children.Add(DockFlyout.Heading(UiText.Get("WidgetNetSpeed")));

        if (!string.IsNullOrEmpty(_primaryInterfaceName))
        {
            panel.Children.Add(DockFlyout.Caption($"{UiText.Get("NetworkAdapter")}: {_primaryInterfaceName}"));
        }

        var speedGrid = new Grid { Margin = new Thickness(0, 8, 0, 8) };
        speedGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        speedGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var downCol = new StackPanel { Margin = new Thickness(0, 0, 4, 0) };
        downCol.Children.Add(new TextBlock
        {
            Text = $"↓ {UiText.Get("NetworkDownload")}",
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = (Brush)FindResource("WidgetAccent")
        });
        downCol.Children.Add(new TextBlock
        {
            Text = FormatSpeed(_downloadBytesPerSec),
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        downCol.Children.Add(new TextBlock
        {
            Text = $"{UiText.Get("NetworkTotalReceived")}:\n{FormatBytes(_sessionDownloadedBytes)}",
            FontSize = 10,
            Foreground = (Brush)FindResource("TextSecondary"),
            Margin = new Thickness(0, 4, 0, 0)
        });

        var upCol = new StackPanel { Margin = new Thickness(4, 0, 0, 0) };
        upCol.Children.Add(new TextBlock
        {
            Text = $"↑ {UiText.Get("NetworkUpload")}",
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = (Brush)FindResource("TextSecondary")
        });
        upCol.Children.Add(new TextBlock
        {
            Text = FormatSpeed(_uploadBytesPerSec),
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        upCol.Children.Add(new TextBlock
        {
            Text = $"{UiText.Get("NetworkTotalSent")}:\n{FormatBytes(_sessionUploadedBytes)}",
            FontSize = 10,
            Foreground = (Brush)FindResource("TextSecondary"),
            Margin = new Thickness(0, 4, 0, 0)
        });

        Grid.SetColumn(downCol, 0);
        Grid.SetColumn(upCol, 1);
        speedGrid.Children.Add(downCol);
        speedGrid.Children.Add(upCol);
        panel.Children.Add(speedGrid);

        return panel;
    }

    internal UIElement BuildClickFlyoutContent()
    {
        _flyoutPanel = new StackPanel { Width = 280 };
        _flyoutPanel.Children.Add(DockFlyout.Heading(UiText.Get("WidgetNetSpeed")));

        if (!string.IsNullOrEmpty(_primaryInterfaceName))
        {
            _flyoutPanel.Children.Add(DockFlyout.Caption($"{UiText.Get("NetworkAdapter")}: {_primaryInterfaceName}"));
        }

        // Live stats grid
        var speedGrid = new Grid { Margin = new Thickness(0, 6, 0, 8) };
        speedGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        speedGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var downCol = new StackPanel { Margin = new Thickness(0, 0, 4, 0) };
        downCol.Children.Add(new TextBlock
        {
            Text = $"↓ {UiText.Get("NetworkDownload")}",
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = (Brush)FindResource("WidgetAccent")
        });
        downCol.Children.Add(new TextBlock
        {
            Text = FormatSpeed(_downloadBytesPerSec),
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        downCol.Children.Add(new TextBlock
        {
            Text = $"{UiText.Get("NetworkTotalReceived")}: {FormatBytes(_sessionDownloadedBytes)}",
            FontSize = 9.5,
            Foreground = (Brush)FindResource("TextSecondary"),
            Margin = new Thickness(0, 2, 0, 0)
        });

        var upCol = new StackPanel { Margin = new Thickness(4, 0, 0, 0) };
        upCol.Children.Add(new TextBlock
        {
            Text = $"↑ {UiText.Get("NetworkUpload")}",
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = (Brush)FindResource("TextSecondary")
        });
        upCol.Children.Add(new TextBlock
        {
            Text = FormatSpeed(_uploadBytesPerSec),
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        upCol.Children.Add(new TextBlock
        {
            Text = $"{UiText.Get("NetworkTotalSent")}: {FormatBytes(_sessionUploadedBytes)}",
            FontSize = 9.5,
            Foreground = (Brush)FindResource("TextSecondary"),
            Margin = new Thickness(0, 2, 0, 0)
        });

        Grid.SetColumn(downCol, 0);
        Grid.SetColumn(upCol, 1);
        speedGrid.Children.Add(downCol);
        speedGrid.Children.Add(upCol);
        _flyoutPanel.Children.Add(speedGrid);

        // Divider
        _flyoutPanel.Children.Add(CreateDivider());

        // Speed Test Section Container
        _speedTestContainer = new StackPanel { Margin = new Thickness(0, 4, 0, 4) };
        UpdateSpeedTestView();
        _flyoutPanel.Children.Add(_speedTestContainer);

        // Divider
        _flyoutPanel.Children.Add(CreateDivider());

        // Settings link
        var settingsButton = DockFlyout.FlatButton(
            new TextBlock { Text = UiText.Get("NetworkSettings"), FontSize = 11.5, FontWeight = FontWeights.SemiBold },
            (_, _) => OpenSettingsAction(),
            UiText.Get("NetworkSettings")
        );
        settingsButton.Margin = new Thickness(0, 2, 0, 0);
        _flyoutPanel.Children.Add(settingsButton);

        return _flyoutPanel;
    }

    private Border CreateDivider()
    {
        var border = new Border
        {
            Height = 1,
            Margin = new Thickness(0, 6, 0, 6)
        };
        border.SetResourceReference(Border.BackgroundProperty, "MenuStroke");
        return border;
    }

    private void UpdateSpeedTestView()
    {
        if (_speedTestContainer == null) return;
        _speedTestContainer.Children.Clear();

        var headerRow = new Grid { Margin = new Thickness(0, 2, 0, 6) };
        headerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        headerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var titleBlock = new TextBlock
        {
            Text = UiText.Get("SpeedTestTitle"),
            FontSize = 12,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };

        var privacyBadge = new TextBlock
        {
            Text = UiText.Get("SpeedTestPrivacyNote"),
            FontSize = 9.5,
            Foreground = (Brush)FindResource("TextSecondary"),
            VerticalAlignment = VerticalAlignment.Center
        };

        Grid.SetColumn(titleBlock, 0);
        Grid.SetColumn(privacyBadge, 1);
        headerRow.Children.Add(titleBlock);
        headerRow.Children.Add(privacyBadge);
        _speedTestContainer.Children.Add(headerRow);

        switch (_testStatus)
        {
            case SpeedTestStatus.Idle:
                if (_lastTestResult != null)
                {
                    _speedTestContainer.Children.Add(CreateResultsGrid(_lastTestResult));
                }

                var startButton = DockFlyout.FlatButton(
                    new TextBlock
                    {
                        Text = _lastTestResult == null ? UiText.Get("SpeedTestStart") : UiText.Get("SpeedTestRestart"),
                        FontWeight = FontWeights.Bold,
                        FontSize = 11.5,
                        Foreground = (Brush)FindResource("WidgetAccent")
                    },
                    async (_, _) => await StartSpeedTestAsync()
                );
                startButton.Margin = new Thickness(0, 4, 0, 0);
                _speedTestContainer.Children.Add(startButton);
                break;

            case SpeedTestStatus.Running:
                var statusBlock = new TextBlock
                {
                    Text = string.IsNullOrEmpty(_testStatusMessage) ? UiText.Get("SpeedTestTestingPing") : _testStatusMessage,
                    FontSize = 11,
                    Foreground = (Brush)FindResource("TextSecondary"),
                    Margin = new Thickness(0, 0, 0, 4)
                };
                _speedTestContainer.Children.Add(statusBlock);

                if (_testLiveMbps > 0)
                {
                    var liveSpeedBlock = new TextBlock
                    {
                        Text = $"{_testLiveMbps.ToString("0.0", CultureInfo.InvariantCulture)} Mbps",
                        FontSize = 18,
                        FontWeight = FontWeights.Bold,
                        Foreground = (Brush)FindResource("WidgetAccent"),
                        Margin = new Thickness(0, 0, 0, 6),
                        Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
                    };
                    _speedTestContainer.Children.Add(liveSpeedBlock);
                }

                var progress = new ProgressBar
                {
                    Height = 4,
                    Minimum = 0,
                    Maximum = 100,
                    Value = _testProgressPercent,
                    Margin = new Thickness(0, 2, 0, 6)
                };
                _speedTestContainer.Children.Add(progress);

                var cancelButton = DockFlyout.FlatButton(
                    new TextBlock { Text = UiText.Get("SpeedTestCancel"), FontSize = 11, Foreground = (Brush)FindResource("TextSecondary") },
                    (_, _) => CancelSpeedTest()
                );
                cancelButton.Margin = new Thickness(0, 2, 0, 0);
                _speedTestContainer.Children.Add(cancelButton);
                break;

            case SpeedTestStatus.Completed:
                if (_lastTestResult != null)
                {
                    _speedTestContainer.Children.Add(CreateResultsGrid(_lastTestResult));
                }

                var testAgainBtn = DockFlyout.FlatButton(
                    new TextBlock
                    {
                        Text = UiText.Get("SpeedTestRestart"),
                        FontWeight = FontWeights.Bold,
                        FontSize = 11.5,
                        Foreground = (Brush)FindResource("WidgetAccent")
                    },
                    async (_, _) => await StartSpeedTestAsync()
                );
                testAgainBtn.Margin = new Thickness(0, 6, 0, 0);
                _speedTestContainer.Children.Add(testAgainBtn);
                break;

            case SpeedTestStatus.Failed:
                var errorBlock = new TextBlock
                {
                    Text = UiText.Get("SpeedTestFailed"),
                    FontSize = 11,
                    Foreground = (Brush)FindResource("WidgetWarning"),
                    TextWrapping = TextWrapping.Wrap,
                    Margin = new Thickness(0, 2, 0, 6)
                };
                _speedTestContainer.Children.Add(errorBlock);

                var retryBtn = DockFlyout.FlatButton(
                    new TextBlock { Text = UiText.Get("SpeedTestRetry"), FontWeight = FontWeights.Bold, FontSize = 11.5 },
                    async (_, _) => await StartSpeedTestAsync()
                );
                retryBtn.Margin = new Thickness(0, 2, 0, 0);
                _speedTestContainer.Children.Add(retryBtn);
                break;
        }
    }

    private Grid CreateResultsGrid(SpeedTestResult result)
    {
        var statGrid = new Grid { Margin = new Thickness(0, 4, 0, 6) };
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        statGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var pingStack = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center };
        pingStack.Children.Add(new TextBlock
        {
            Text = UiText.Get("SpeedTestPing"),
            FontSize = 10,
            Foreground = (Brush)FindResource("TextSecondary")
        });
        pingStack.Children.Add(new TextBlock
        {
            Text = result.PingMs >= 0 ? $"{result.PingMs} ms" : "—",
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });

        var downStack = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center };
        downStack.Children.Add(new TextBlock
        {
            Text = $"↓ {UiText.Get("NetworkDownload")}",
            FontSize = 10,
            Foreground = (Brush)FindResource("WidgetAccent")
        });
        downStack.Children.Add(new TextBlock
        {
            Text = $"{result.DownloadMbps.ToString("0.0", CultureInfo.InvariantCulture)}",
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        downStack.Children.Add(new TextBlock
        {
            Text = "Mbps",
            FontSize = 9,
            Foreground = (Brush)FindResource("TextSecondary")
        });

        var upStack = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center };
        upStack.Children.Add(new TextBlock
        {
            Text = $"↑ {UiText.Get("NetworkUpload")}",
            FontSize = 10,
            Foreground = (Brush)FindResource("TextSecondary")
        });
        upStack.Children.Add(new TextBlock
        {
            Text = $"{result.UploadMbps.ToString("0.0", CultureInfo.InvariantCulture)}",
            FontSize = 13,
            FontWeight = FontWeights.Bold,
            Typography = { NumeralAlignment = FontNumeralAlignment.Tabular }
        });
        upStack.Children.Add(new TextBlock
        {
            Text = "Mbps",
            FontSize = 9,
            Foreground = (Brush)FindResource("TextSecondary")
        });

        Grid.SetColumn(pingStack, 0);
        Grid.SetColumn(downStack, 1);
        Grid.SetColumn(upStack, 2);

        statGrid.Children.Add(pingStack);
        statGrid.Children.Add(downStack);
        statGrid.Children.Add(upStack);
        return statGrid;
    }

    internal async Task StartSpeedTestAsync()
    {
        CancelSpeedTest();
        _testCts = new CancellationTokenSource();
        var ct = _testCts.Token;

        _testStatus = SpeedTestStatus.Running;
        _testStatusMessage = UiText.Get("SpeedTestTestingPing");
        _testLiveMbps = 0;
        _testProgressPercent = 5;
        UpdateSpeedTestView();

        var progress = new Progress<SpeedTestProgress>(p =>
        {
            if (ct.IsCancellationRequested) return;
            _testStatusMessage = p.StatusText;
            _testLiveMbps = p.CurrentMbps;
            _testProgressPercent = p.Percent;
            UpdateSpeedTestView();
        });

        try
        {
            var result = await SpeedTestRunner(progress, ct).ConfigureAwait(true);
            if (!ct.IsCancellationRequested)
            {
                _lastTestResult = result;
                _testStatus = SpeedTestStatus.Completed;
            }
        }
        catch (OperationCanceledException)
        {
            _testStatus = SpeedTestStatus.Idle;
        }
        catch
        {
            _testStatus = SpeedTestStatus.Failed;
        }
        finally
        {
            _testLiveMbps = 0;
            UpdateSpeedTestView();
        }
    }

    internal void CancelSpeedTest()
    {
        if (_testCts != null)
        {
            try
            {
                _testCts.Cancel();
                _testCts.Dispose();
            }
            catch
            {
            }
            _testCts = null;
        }
        _testStatus = SpeedTestStatus.Idle;
        UpdateSpeedTestView();
    }

    private static async Task<SpeedTestResult> RunMultiStreamSpeedTestAsync(IProgress<SpeedTestProgress> progress, CancellationToken ct)
    {
        // 1. Measure latency
        progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingPing"), 0, 10));
        var pingMs = await MeasurePingAsync(ct).ConfigureAwait(false);

        // 2. Measure download using 6 concurrent streams over 5 seconds
        progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingDownload"), 0, 15));
        var downMbps = await MeasureMultiStreamDownloadAsync(progress, ct).ConfigureAwait(false);

        // 3. Measure upload using 4 concurrent streams over 4 seconds
        progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingUpload"), downMbps, 60));
        var upMbps = await MeasureMultiStreamUploadAsync(progress, ct).ConfigureAwait(false);

        progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingUpload"), upMbps, 100));
        return new SpeedTestResult(pingMs, downMbps, upMbps);
    }

    private static async Task<int> MeasurePingAsync(CancellationToken ct)
    {
        int minPing = int.MaxValue;
        for (int i = 0; i < 3; i++)
        {
            ct.ThrowIfCancellationRequested();
            try
            {
                using var ping = new Ping();
                var reply = await ping.SendPingAsync("1.1.1.1", 1000).ConfigureAwait(false);
                if (reply.Status == IPStatus.Success && reply.RoundtripTime < minPing)
                {
                    minPing = (int)reply.RoundtripTime;
                }
            }
            catch
            {
            }
        }

        if (minPing < int.MaxValue) return minPing;

        try
        {
            var sw = Stopwatch.StartNew();
            using var request = new HttpRequestMessage(HttpMethod.Head, "https://1.1.1.1");
            using var response = await SharedHttpClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
            sw.Stop();
            return Math.Max(1, (int)sw.ElapsedMilliseconds);
        }
        catch
        {
            return -1;
        }
    }

    private static async Task<double> MeasureMultiStreamDownloadAsync(IProgress<SpeedTestProgress> progress, CancellationToken ct)
    {
        const int StreamCount = 6;
        const double TestDurationSec = 5.0;

        using var phaseCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        phaseCts.CancelAfter(TimeSpan.FromSeconds(TestDurationSec));

        long totalBytes = 0;
        var sw = Stopwatch.StartNew();

        var downloadTasks = new Task[StreamCount];
        for (int i = 0; i < StreamCount; i++)
        {
            downloadTasks[i] = Task.Run(async () =>
            {
                var buffer = new byte[64 * 1024];
                while (!phaseCts.IsCancellationRequested)
                {
                    try
                    {
                        var url = "https://speed.cloudflare.com/__down?bytes=25000000";
                        using var response = await SharedHttpClient.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, phaseCts.Token).ConfigureAwait(false);
                        if (!response.IsSuccessStatusCode) break;

                        using var stream = await response.Content.ReadAsStreamAsync(phaseCts.Token).ConfigureAwait(false);
                        while (!phaseCts.IsCancellationRequested)
                        {
                            var read = await stream.ReadAsync(buffer, 0, buffer.Length, phaseCts.Token).ConfigureAwait(false);
                            if (read == 0) break;
                            Interlocked.Add(ref totalBytes, read);
                        }
                    }
                    catch (OperationCanceledException)
                    {
                        break;
                    }
                    catch
                    {
                        break;
                    }
                }
            }, phaseCts.Token);
        }

        var progressTask = Task.Run(async () =>
        {
            while (!phaseCts.IsCancellationRequested)
            {
                await Task.Delay(100).ConfigureAwait(false);
                var sec = sw.Elapsed.TotalSeconds;
                if (sec > 0.1)
                {
                    var currentTotal = Interlocked.Read(ref totalBytes);
                    var mbps = (currentTotal * 8.0) / (sec * 1_000_000.0);
                    var percent = 15.0 + Math.Min(45.0, (sec / TestDurationSec) * 45.0);
                    progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingDownload"), mbps, percent));
                }
            }
        });

        try
        {
            await Task.WhenAll(downloadTasks).ConfigureAwait(false);
        }
        catch
        {
        }

        try
        {
            await progressTask.ConfigureAwait(false);
        }
        catch
        {
        }

        sw.Stop();
        var elapsedSec = Math.Max(0.2, sw.Elapsed.TotalSeconds);
        var finalTotalBytes = Interlocked.Read(ref totalBytes);
        return (finalTotalBytes * 8.0) / (elapsedSec * 1_000_000.0);
    }

    private static async Task<double> MeasureMultiStreamUploadAsync(IProgress<SpeedTestProgress> progress, CancellationToken ct)
    {
        const int StreamCount = 4;
        const double TestDurationSec = 4.0;

        using var phaseCts = CancellationTokenSource.CreateLinkedTokenSource(ct);
        phaseCts.CancelAfter(TimeSpan.FromSeconds(TestDurationSec));

        long totalBytes = 0;
        var sw = Stopwatch.StartNew();

        var uploadTasks = new Task[StreamCount];
        for (int i = 0; i < StreamCount; i++)
        {
            uploadTasks[i] = Task.Run(async () =>
            {
                while (!phaseCts.IsCancellationRequested)
                {
                    try
                    {
                        using var content = new ByteArrayContent(UploadPayloadChunk);
                        using var response = await SharedHttpClient.PostAsync("https://speed.cloudflare.com/__up", content, phaseCts.Token).ConfigureAwait(false);
                        if (!response.IsSuccessStatusCode) break;
                        Interlocked.Add(ref totalBytes, UploadPayloadChunk.Length);
                    }
                    catch (OperationCanceledException)
                    {
                        break;
                    }
                    catch
                    {
                        break;
                    }
                }
            }, phaseCts.Token);
        }

        var progressTask = Task.Run(async () =>
        {
            while (!phaseCts.IsCancellationRequested)
            {
                await Task.Delay(100).ConfigureAwait(false);
                var sec = sw.Elapsed.TotalSeconds;
                if (sec > 0.1)
                {
                    var currentTotal = Interlocked.Read(ref totalBytes);
                    var mbps = (currentTotal * 8.0) / (sec * 1_000_000.0);
                    var percent = 60.0 + Math.Min(40.0, (sec / TestDurationSec) * 40.0);
                    progress.Report(new SpeedTestProgress(UiText.Get("SpeedTestTestingUpload"), mbps, percent));
                }
            }
        });

        try
        {
            await Task.WhenAll(uploadTasks).ConfigureAwait(false);
        }
        catch
        {
        }

        try
        {
            await progressTask.ConfigureAwait(false);
        }
        catch
        {
        }

        sw.Stop();
        var elapsedSec = Math.Max(0.2, sw.Elapsed.TotalSeconds);
        var finalTotalBytes = Interlocked.Read(ref totalBytes);
        return (finalTotalBytes * 8.0) / (elapsedSec * 1_000_000.0);
    }

    public static string FormatSpeed(double bytesPerSecond)
    {
        if (bytesPerSecond < 1024)
        {
            return $"{(int)Math.Max(0, bytesPerSecond)} B/s";
        }

        var kb = bytesPerSecond / 1024.0;
        if (kb < 1024)
        {
            return kb < 100
                ? $"{kb.ToString("0.0", CultureInfo.InvariantCulture)} KB/s"
                : $"{(int)kb} KB/s";
        }

        var mb = kb / 1024.0;
        if (mb < 1024)
        {
            return mb < 100
                ? $"{mb.ToString("0.0", CultureInfo.InvariantCulture)} MB/s"
                : $"{(int)mb} MB/s";
        }

        var gb = mb / 1024.0;
        return $"{gb.ToString("0.00", CultureInfo.InvariantCulture)} GB/s";
    }

    public static string FormatCompactSpeed(double bytesPerSecond)
    {
        if (bytesPerSecond < 1024)
        {
            return "0";
        }

        var kb = bytesPerSecond / 1024.0;
        if (kb < 1024)
        {
            return $"{(int)kb}K";
        }

        var mb = kb / 1024.0;
        if (mb < 1024)
        {
            return mb < 10
                ? $"{mb.ToString("0.0", CultureInfo.InvariantCulture)}M"
                : $"{(int)mb}M";
        }

        var gb = mb / 1024.0;
        return $"{gb.ToString("0.0", CultureInfo.InvariantCulture)}G";
    }

    public static string FormatBytes(long bytes)
    {
        if (bytes < 1024)
        {
            return $"{Math.Max(0, bytes)} B";
        }

        var kb = bytes / 1024.0;
        if (kb < 1024)
        {
            return $"{kb.ToString("0.0", CultureInfo.InvariantCulture)} KB";
        }

        var mb = kb / 1024.0;
        if (mb < 1024)
        {
            return $"{mb.ToString("0.0", CultureInfo.InvariantCulture)} MB";
        }

        var gb = mb / 1024.0;
        return $"{gb.ToString("0.00", CultureInfo.InvariantCulture)} GB";
    }

    private static (long Received, long Sent, string PrimaryInterfaceName) ReadNetworkBytes()
    {
        long totalReceived = 0;
        long totalSent = 0;
        string primaryName = string.Empty;
        long maxTraffic = -1;

        try
        {
            var interfaces = NetworkInterface.GetAllNetworkInterfaces();
            foreach (var ni in interfaces)
            {
                if (ni.OperationalStatus != OperationalStatus.Up)
                    continue;
                if (ni.NetworkInterfaceType is NetworkInterfaceType.Loopback or NetworkInterfaceType.Tunnel)
                    continue;

                var stats = ni.GetIPStatistics();
                totalReceived += stats.BytesReceived;
                totalSent += stats.BytesSent;

                var traffic = stats.BytesReceived + stats.BytesSent;
                if (traffic > maxTraffic)
                {
                    maxTraffic = traffic;
                    primaryName = ni.Name;
                }
            }
        }
        catch
        {
        }

        return (totalReceived, totalSent, primaryName);
    }

    private static void DefaultOpenSettings()
    {
        DockFlyout.CloseAll();
        try
        {
            Process.Start(new ProcessStartInfo("ms-settings:network") { UseShellExecute = true });
        }
        catch
        {
            try
            {
                Process.Start(new ProcessStartInfo("control.exe", "netconnections") { UseShellExecute = true });
            }
            catch
            {
            }
        }
    }
}
