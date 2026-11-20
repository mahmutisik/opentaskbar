using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

public partial class AudioWidget : DockWidget
{
    private readonly DockFlyout _flyout = new();
    private AudioEndpointMonitor.SharedHandle? _monitor;
    private bool _updating;
    private bool _updatingMic;
    private bool? _lastHasDevice;
    private int _lastLevel = -1;
    private bool _lastMuted;
    private string? _lastDeviceName;
    private string? _lastIconKey;
    private AppLanguage _lastLanguage;

    private bool _outputDevicesExpanded;
    private bool _inputDevicesExpanded;

    private Border? _flyoutContainer;
    private Slider? _flyoutSlider;
    private TextBlock? _flyoutLevelText;
    private ContentControl? _flyoutIcon;
    private Slider? _flyoutMicSlider;
    private TextBlock? _flyoutMicLevelText;
    private ContentControl? _flyoutMicIcon;
    private readonly List<AppAudioSession> _appSessions = [];

    // Allows the mixer controls to be tested without real audio hardware.
    internal Func<IReadOnlyList<AppAudioSession>>? AppSessionSource { get; set; }

    private void ReleaseAppSessions()
    {
        foreach (var session in _appSessions) session.Dispose();
        _appSessions.Clear();
    }

    public override bool SupportsCompactMode => true;
    public override int ShrinkPriority => 40;
    public override double FullWidthEstimate => 140;
    public override double CompactWidthEstimate => 48;

    public AudioWidget()
    {
        InitializeComponent();
    }

    protected override void OnCompactChanged(bool compact) => ApplyDensityLayout();
    protected override void OnHoverExpandedChanged(bool expanded) => ApplyDensityLayout();

    private void ApplyDensityLayout()
    {
        var compact = IsCompact && !IsHoverExpanded;
        Card.Width = compact ? 48 : 140;
        Card.Padding = compact ? new Thickness(4, 2, 4, 2) : new Thickness(8, 0, 10, 0);
        DeviceText.Visibility = compact ? Visibility.Collapsed : Visibility.Visible;

        if (compact)
        {
            MuteButton.Margin = new Thickness(0);
            MuteButton.Width = 22;
            MuteButton.Height = 20;
            LevelText.FontSize = 10;
        }
        else
        {
            MuteButton.Margin = new Thickness(0, 0, 4, 0);
            MuteButton.Width = 24;
            MuteButton.Height = 24;
            LevelText.ClearValue(TextBlock.FontSizeProperty);
        }
    }

    public override UIElement? BuildHoverFlyoutContent() => BuildFlyoutContent();
    public override UIElement? BuildFullHoverFlyoutContent() => BuildFlyoutContent();

    public UIElement BuildFlyoutContent()
    {
        var container = new Border { Width = 310 };
        _flyoutContainer = container;
        container.Unloaded += (_, _) =>
        {
            if (!ReferenceEquals(_flyoutContainer, container)) return;
            ReleaseAppSessions();
            _flyoutContainer = null;
            _flyoutSlider = null;
            _flyoutLevelText = null;
            _flyoutIcon = null;
            _flyoutMicSlider = null;
            _flyoutMicLevelText = null;
            _flyoutMicIcon = null;
            _outputDevicesExpanded = false;
            _inputDevicesExpanded = false;
        };

        PopulateFlyout(container);
        return container;
    }

    private void PopulateFlyout(Border container)
    {
        ReleaseAppSessions();
        var root = new StackPanel { Margin = new Thickness(0) };

        // Header: Ses Denetimi
        var header = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(4, 0, 4, 10) };
        var headerIcon = new ContentControl
        {
            Width = 15,
            Height = 15,
            Content = FindResource("IconSliders"),
            Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        headerIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var headerTitle = new TextBlock
        {
            Text = UiText.Get("Volume"),
            FontWeight = FontWeights.SemiBold,
            FontSize = 12.5,
            VerticalAlignment = VerticalAlignment.Center
        };
        headerTitle.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        header.Children.Add(headerIcon);
        header.Children.Add(headerTitle);
        root.Children.Add(header);

        // 1. Çıkış Sesi Kartı (Output Card)
        var outCard = CreateSectionCard();
        var outCardPanel = new StackPanel();

        var outDevices = _monitor?.GetPlaybackDevices() ?? [];
        var activeOutDev = outDevices.FirstOrDefault(d => d.IsDefault) ?? (outDevices.Count > 0 ? outDevices[0] : null);

        var isHeadphone = activeOutDev is not null && (
            activeOutDev.Name.Contains("headphone", StringComparison.OrdinalIgnoreCase)
            || activeOutDev.Name.Contains("kulaklık", StringComparison.OrdinalIgnoreCase)
            || activeOutDev.Name.Contains("headset", StringComparison.OrdinalIgnoreCase)
            || activeOutDev.Name.Contains("buds", StringComparison.OrdinalIgnoreCase)
            || activeOutDev.Name.Contains("airpod", StringComparison.OrdinalIgnoreCase));

        // Active device header row (clickable to expand if multiple devices)
        var outDevRow = new Grid
        {
            Cursor = outDevices.Count > 1 ? Cursors.Hand : Cursors.Arrow,
            Background = Brushes.Transparent,
            Margin = new Thickness(0, 0, 0, 6)
        };
        outDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        outDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        outDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var outDevIcon = new ContentControl
        {
            Width = 14,
            Height = 14,
            Content = FindResource(isHeadphone ? "IconHeadphones" : "IconVolume"),
            Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        outDevIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var outDevName = new TextBlock
        {
            Text = activeOutDev?.Name ?? UiText.Get("NoAudioDevice"),
            FontWeight = FontWeights.SemiBold,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        outDevName.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        Grid.SetColumn(outDevIcon, 0);
        Grid.SetColumn(outDevName, 1);
        outDevRow.Children.Add(outDevIcon);
        outDevRow.Children.Add(outDevName);

        if (outDevices.Count > 1)
        {
            var chevron = new ContentControl
            {
                Width = 12,
                Height = 12,
                Content = FindResource(_outputDevicesExpanded ? "IconChevronDown" : "IconChevronNext"),
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(6, 0, 0, 0)
            };
            chevron.SetResourceReference(StyleProperty, "WidgetIcon");
            Grid.SetColumn(chevron, 2);
            outDevRow.Children.Add(chevron);

            outDevRow.MouseLeftButtonUp += (_, _) =>
            {
                _outputDevicesExpanded = !_outputDevicesExpanded;
                if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
            };
        }

        outCardPanel.Children.Add(outDevRow);

        // Expanded device list
        if (_outputDevicesExpanded && outDevices.Count > 1)
        {
            var devList = new StackPanel { Margin = new Thickness(0, 2, 0, 6) };
            foreach (var dev in outDevices)
            {
                var isSelected = dev.IsDefault;
                var itemBtn = new Button { Cursor = Cursors.Hand, Margin = new Thickness(0, 1, 0, 1), Padding = new Thickness(6, 4, 6, 4) };
                itemBtn.SetResourceReference(StyleProperty, "FlyoutButton");

                var itemGrid = new Grid();
                itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(20, GridUnitType.Pixel) });
                itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

                if (isSelected)
                {
                    var check = new ContentControl
                    {
                        Width = 12,
                        Height = 12,
                        Content = FindResource("IconCheck"),
                        VerticalAlignment = VerticalAlignment.Center
                    };
                    check.SetResourceReference(ContentControl.ForegroundProperty, "WidgetAccent");
                    Grid.SetColumn(check, 0);
                    itemGrid.Children.Add(check);
                }

                var label = new TextBlock
                {
                    Text = dev.Name,
                    FontSize = 11.5,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextTrimming = TextTrimming.CharacterEllipsis
                };
                if (isSelected)
                {
                    label.FontWeight = FontWeights.SemiBold;
                    label.SetResourceReference(TextBlock.ForegroundProperty, "WidgetAccent");
                }
                else
                {
                    label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
                }
                Grid.SetColumn(label, 1);
                itemGrid.Children.Add(label);

                itemBtn.Content = itemGrid;
                var devId = dev.Id;
                itemBtn.Click += (_, _) =>
                {
                    if (_monitor is null) return;
                    _monitor.SelectDefaultDevice(devId);
                    _monitor.BindDefaultDevice();
                    _outputDevicesExpanded = false;
                    Render();
                    if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
                };
                devList.Children.Add(itemBtn);
            }
            outCardPanel.Children.Add(devList);
        }

        // Master Volume Slider Row
        var hasOutDevice = _monitor?.HasDevice == true;
        var outLevel = hasOutDevice ? (int)Math.Round(_monitor!.Volume * 100) : 0;
        var outMuted = _monitor?.Muted == true;

        var outSliderRow = new Grid { Margin = new Thickness(0, 2, 0, 2) };
        outSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        outSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        outSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var outIconKey = outMuted || outLevel == 0 ? "IconVolumeMute" : outLevel < 50 ? "IconVolumeLow" : "IconVolume";
        var outIcon = new ContentControl { Width = 15, Height = 15, Content = FindResource(outIconKey) };
        outIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var outMuteBtn = new Button
        {
            Width = 26,
            Height = 26,
            Content = outIcon,
            IsEnabled = hasOutDevice,
            ToolTip = UiText.Get("MuteToggle")
        };
        outMuteBtn.SetResourceReference(StyleProperty, "WidgetIconButton");
        outMuteBtn.Click += MuteButton_Click;

        var outSlider = new Slider
        {
            Minimum = 0,
            Maximum = 100,
            Value = outLevel,
            IsEnabled = hasOutDevice,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(8, 0, 8, 0)
        };
        outSlider.SetResourceReference(StyleProperty, "WidgetSlider");
        outSlider.ValueChanged += (_, args) =>
        {
            if (_updating || _monitor is null) return;
            if (_monitor.SetVolume((float)(args.NewValue / 100.0))) Render();
            else Report(UiText.Get("ErrorVolumeChange"));
        };

        var outLevelText = new TextBlock
        {
            Text = hasOutDevice ? (outMuted ? "—" : UiText.Format("Percent", outLevel)) : string.Empty,
            VerticalAlignment = VerticalAlignment.Center,
            MinWidth = 34,
            FontWeight = FontWeights.SemiBold,
            FontSize = 11.5,
            TextAlignment = TextAlignment.Right
        };
        outLevelText.SetResourceReference(StyleProperty, "WidgetCaption");

        Grid.SetColumn(outMuteBtn, 0);
        Grid.SetColumn(outSlider, 1);
        Grid.SetColumn(outLevelText, 2);
        outSliderRow.Children.Add(outMuteBtn);
        outSliderRow.Children.Add(outSlider);
        outSliderRow.Children.Add(outLevelText);
        outCardPanel.Children.Add(outSliderRow);

        _flyoutIcon = outIcon;
        _flyoutSlider = outSlider;
        _flyoutLevelText = outLevelText;

        outCard.Child = outCardPanel;
        root.Children.Add(outCard);

        // 2. Giriş Sesi / Mikrofon Kartı (Microphone Card)
        var micCard = CreateSectionCard();
        var micCardPanel = new StackPanel();

        var inDevices = _monitor?.GetRecordingDevices() ?? [];
        var activeInDev = inDevices.FirstOrDefault(d => d.IsDefault) ?? (inDevices.Count > 0 ? inDevices[0] : null);
        var hasMic = _monitor?.HasMicDevice == true;

        var inDevRow = new Grid
        {
            Cursor = inDevices.Count > 1 ? Cursors.Hand : Cursors.Arrow,
            Background = Brushes.Transparent,
            Margin = new Thickness(0, 0, 0, 6)
        };
        inDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        inDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        inDevRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var inDevIcon = new ContentControl
        {
            Width = 14,
            Height = 14,
            Content = FindResource("IconMic"),
            Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        inDevIcon.SetResourceReference(StyleProperty, "WidgetIcon");

        var inDevName = new TextBlock
        {
            Text = activeInDev?.Name ?? UiText.Get("NoMicDevice"),
            FontWeight = FontWeights.SemiBold,
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis
        };
        inDevName.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

        Grid.SetColumn(inDevIcon, 0);
        Grid.SetColumn(inDevName, 1);
        inDevRow.Children.Add(inDevIcon);
        inDevRow.Children.Add(inDevName);

        if (inDevices.Count > 1)
        {
            var chevron = new ContentControl
            {
                Width = 12,
                Height = 12,
                Content = FindResource(_inputDevicesExpanded ? "IconChevronDown" : "IconChevronNext"),
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(6, 0, 0, 0)
            };
            chevron.SetResourceReference(StyleProperty, "WidgetIcon");
            Grid.SetColumn(chevron, 2);
            inDevRow.Children.Add(chevron);

            inDevRow.MouseLeftButtonUp += (_, _) =>
            {
                _inputDevicesExpanded = !_inputDevicesExpanded;
                if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
            };
        }

        micCardPanel.Children.Add(inDevRow);

        // Expanded mic list
        if (_inputDevicesExpanded && inDevices.Count > 1)
        {
            var devList = new StackPanel { Margin = new Thickness(0, 2, 0, 6) };
            foreach (var dev in inDevices)
            {
                var isSelected = dev.IsDefault;
                var itemBtn = new Button { Cursor = Cursors.Hand, Margin = new Thickness(0, 1, 0, 1), Padding = new Thickness(6, 4, 6, 4) };
                itemBtn.SetResourceReference(StyleProperty, "FlyoutButton");

                var itemGrid = new Grid();
                itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(20, GridUnitType.Pixel) });
                itemGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

                if (isSelected)
                {
                    var check = new ContentControl
                    {
                        Width = 12,
                        Height = 12,
                        Content = FindResource("IconCheck"),
                        VerticalAlignment = VerticalAlignment.Center
                    };
                    check.SetResourceReference(ContentControl.ForegroundProperty, "WidgetAccent");
                    Grid.SetColumn(check, 0);
                    itemGrid.Children.Add(check);
                }

                var label = new TextBlock
                {
                    Text = dev.Name,
                    FontSize = 11.5,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextTrimming = TextTrimming.CharacterEllipsis
                };
                if (isSelected)
                {
                    label.FontWeight = FontWeights.SemiBold;
                    label.SetResourceReference(TextBlock.ForegroundProperty, "WidgetAccent");
                }
                else
                {
                    label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
                }
                Grid.SetColumn(label, 1);
                itemGrid.Children.Add(label);

                itemBtn.Content = itemGrid;
                var devId = dev.Id;
                itemBtn.Click += (_, _) =>
                {
                    if (_monitor is null) return;
                    _monitor.SelectDefaultDevice(devId);
                    _monitor.BindDefaultMicDevice();
                    _inputDevicesExpanded = false;
                    RenderMic();
                    if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
                };
                devList.Children.Add(itemBtn);
            }
            micCardPanel.Children.Add(devList);
        }

        // Mic Volume Slider Row
        if (hasMic)
        {
            var micLevel = (int)Math.Round(_monitor!.MicVolume * 100);
            var micMuted = _monitor.MicMuted;

            var micSliderRow = new Grid { Margin = new Thickness(0, 2, 0, 2) };
            micSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            micSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            micSliderRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var micIcon = new ContentControl
            {
                Width = 15,
                Height = 15,
                Content = FindResource(micMuted ? "IconMicMute" : "IconMic")
            };
            micIcon.SetResourceReference(StyleProperty, "WidgetIcon");

            var micMuteBtn = new Button
            {
                Width = 26,
                Height = 26,
                Content = micIcon,
                ToolTip = UiText.Get("MicMuteToggle")
            };
            micMuteBtn.SetResourceReference(StyleProperty, "WidgetIconButton");
            micMuteBtn.Click += (_, _) =>
            {
                if (_monitor is null) return;
                if (_monitor.ToggleMicMute()) RenderMic();
            };

            var micSlider = new Slider
            {
                Minimum = 0,
                Maximum = 100,
                Value = micLevel,
                VerticalAlignment = VerticalAlignment.Center,
                Margin = new Thickness(8, 0, 8, 0)
            };
            micSlider.SetResourceReference(StyleProperty, "WidgetSlider");
            micSlider.ValueChanged += (_, args) =>
            {
                if (_updatingMic || _monitor is null) return;
                if (_monitor.SetMicVolume((float)(args.NewValue / 100.0))) RenderMic();
            };

            var micLevelText = new TextBlock
            {
                Text = micMuted ? "—" : UiText.Format("Percent", micLevel),
                VerticalAlignment = VerticalAlignment.Center,
                MinWidth = 34,
                FontWeight = FontWeights.SemiBold,
                FontSize = 11.5,
                TextAlignment = TextAlignment.Right
            };
            micLevelText.SetResourceReference(StyleProperty, "WidgetCaption");

            Grid.SetColumn(micMuteBtn, 0);
            Grid.SetColumn(micSlider, 1);
            Grid.SetColumn(micLevelText, 2);
            micSliderRow.Children.Add(micMuteBtn);
            micSliderRow.Children.Add(micSlider);
            micSliderRow.Children.Add(micLevelText);
            micCardPanel.Children.Add(micSliderRow);

            _flyoutMicIcon = micIcon;
            _flyoutMicSlider = micSlider;
            _flyoutMicLevelText = micLevelText;
        }

        micCard.Child = micCardPanel;
        root.Children.Add(micCard);

        // 3. Uygulama Sesleri Kartı (App Mixer Card)
        var appCard = CreateSectionCard();
        appCard.Margin = new Thickness(0);
        var appCardPanel = new StackPanel();

        var appHeader = new TextBlock
        {
            Text = UiText.Get("AppVolumeMixer"),
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 6)
        };
        appHeader.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        appCardPanel.Children.Add(appHeader);

        var sessions = AppSessionSource?.Invoke() ?? _monitor?.GetAppSessions() ?? [];
        _appSessions.AddRange(sessions);
        if (sessions.Count == 0)
        {
            var emptyText = new TextBlock
            {
                Text = UiText.Get("NoActiveAudioApps"),
                FontSize = 11.5,
                Margin = new Thickness(2, 4, 2, 4)
            };
            emptyText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            appCardPanel.Children.Add(emptyText);
        }
        else
        {
            var appList = new StackPanel();
            foreach (var session in sessions)
            {
                var appRow = new Grid { Margin = new Thickness(0, 3, 0, 3) };
                appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(22, GridUnitType.Pixel) });
                appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(74, GridUnitType.Pixel) });
                appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(26, GridUnitType.Pixel) });
                appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                appRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32, GridUnitType.Pixel) });

                UIElement appIconEl;
                if (session.Icon is not null)
                {
                    appIconEl = new Image
                    {
                        Source = session.Icon,
                        Width = 16,
                        Height = 16,
                        VerticalAlignment = VerticalAlignment.Center,
                        HorizontalAlignment = HorizontalAlignment.Left
                    };
                }
                else
                {
                    var ic = new ContentControl
                    {
                        Width = 14,
                        Height = 14,
                        Content = FindResource("IconSliders"),
                        VerticalAlignment = VerticalAlignment.Center,
                        HorizontalAlignment = HorizontalAlignment.Left
                    };
                    ic.SetResourceReference(StyleProperty, "WidgetIcon");
                    appIconEl = ic;
                }

                var nameBlock = new TextBlock
                {
                    Text = session.Name,
                    FontSize = 11,
                    TextTrimming = TextTrimming.CharacterEllipsis,
                    VerticalAlignment = VerticalAlignment.Center,
                    ToolTip = session.Name,
                    Margin = new Thickness(2, 0, 4, 0)
                };
                nameBlock.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

                var currentMuted = session.IsMuted;
                var appVol = (int)Math.Round(session.Volume * 100);

                var appIcon = new ContentControl
                {
                    Width = 13,
                    Height = 13,
                    Content = FindResource(currentMuted || appVol == 0 ? "IconVolumeMute" : "IconVolume")
                };
                appIcon.SetResourceReference(StyleProperty, "WidgetIcon");

                var appMuteBtn = new Button
                {
                    Width = 22,
                    Height = 22,
                    Content = appIcon,
                    ToolTip = UiText.Get("MuteToggle")
                };
                appMuteBtn.SetResourceReference(StyleProperty, "WidgetIconButton");

                var appPercent = new TextBlock
                {
                    Text = currentMuted ? "—" : UiText.Format("Percent", appVol),
                    FontSize = 10.5,
                    VerticalAlignment = VerticalAlignment.Center,
                    TextAlignment = TextAlignment.Right
                };
                appPercent.SetResourceReference(StyleProperty, "WidgetCaption");

                var appSlider = new Slider
                {
                    Minimum = 0,
                    Maximum = 100,
                    Value = appVol,
                    VerticalAlignment = VerticalAlignment.Center,
                    Margin = new Thickness(4, 0, 4, 0)
                };
                appSlider.SetResourceReference(StyleProperty, "WidgetSlider");

                var localSession = session;
                var appUpdating = false;
                var confirmedLevel = appVol;

                appSlider.ValueChanged += (_, args) =>
                {
                    if (appUpdating) return;
                    var newLevel = (int)Math.Round(args.NewValue);
                    if (!localSession.SetVolume(newLevel / 100f))
                    {
                        appUpdating = true;
                        try { appSlider.Value = confirmedLevel; }
                        finally { appUpdating = false; }
                        Report(UiText.Get("ErrorVolumeChange"));
                        return;
                    }

                    confirmedLevel = newLevel;
                    if (currentMuted && newLevel > 0)
                    {
                        if (localSession.SetMute(false)) currentMuted = false;
                        else Report(UiText.Get("ErrorVolumeChange"));
                    }
                    appIcon.Content = FindResource(currentMuted || newLevel == 0 ? "IconVolumeMute" : "IconVolume");
                    appPercent.Text = currentMuted ? "—" : UiText.Format("Percent", newLevel);
                };

                appMuteBtn.Click += (_, _) =>
                {
                    var mute = !currentMuted;
                    if (!localSession.SetMute(mute))
                    {
                        Report(UiText.Get("ErrorVolumeChange"));
                        return;
                    }
                    currentMuted = mute;
                    appIcon.Content = FindResource(currentMuted || (int)appSlider.Value == 0 ? "IconVolumeMute" : "IconVolume");
                    appPercent.Text = currentMuted ? "—" : UiText.Format("Percent", (int)Math.Round(appSlider.Value));
                };

                Grid.SetColumn(appIconEl, 0);
                Grid.SetColumn(nameBlock, 1);
                Grid.SetColumn(appMuteBtn, 2);
                Grid.SetColumn(appSlider, 3);
                Grid.SetColumn(appPercent, 4);

                appRow.Children.Add(appIconEl);
                appRow.Children.Add(nameBlock);
                appRow.Children.Add(appMuteBtn);
                appRow.Children.Add(appSlider);
                appRow.Children.Add(appPercent);
                appList.Children.Add(appRow);
            }

            var scrollViewer = new ScrollViewer
            {
                Content = appList,
                MaxHeight = 150,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
                Focusable = false
            };
            appCardPanel.Children.Add(scrollViewer);
        }

        appCard.Child = appCardPanel;
        root.Children.Add(appCard);

        container.Child = root;
    }

    private static Border CreateSectionCard()
    {
        var border = new Border
        {
            CornerRadius = new CornerRadius(10),
            BorderThickness = new Thickness(1),
            Padding = new Thickness(10, 8, 10, 8),
            Margin = new Thickness(0, 0, 0, 8)
        };
        border.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
        border.SetResourceReference(Border.BorderBrushProperty, "StrokeSubtle");
        return border;
    }

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        try
        {
            _monitor = AudioEndpointMonitor.AcquireShared();
            _monitor.VolumeChanged += Monitor_VolumeChanged;
            _monitor.DefaultDeviceChanged += Monitor_DefaultDeviceChanged;
            _monitor.MicVolumeChanged += Monitor_MicVolumeChanged;
            _monitor.DefaultMicChanged += Monitor_DefaultMicChanged;
        }
        catch (COMException ex)
        {
            AppLog.Warning($"Core Audio unavailable: {ex.Message}");
            _monitor = null;
        }

        InvalidateCache();
        Render();
    }

    protected override void OnDeactivated()
    {
        _flyout.Close();
        ReleaseAppSessions();
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        if (_monitor is null) return;
        _monitor.VolumeChanged -= Monitor_VolumeChanged;
        _monitor.DefaultDeviceChanged -= Monitor_DefaultDeviceChanged;
        _monitor.MicVolumeChanged -= Monitor_MicVolumeChanged;
        _monitor.DefaultMicChanged -= Monitor_DefaultMicChanged;
        _monitor.Dispose();
        _monitor = null;
    }

    protected override void OnDisposed()
    {
        _flyout.Close();
        ReleaseAppSessions();
    }

    private void UiText_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        Render();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    }

    private void InvalidateCache()
    {
        _lastHasDevice = null;
        _lastLevel = -1;
        _lastIconKey = null;
        _lastDeviceName = null;
    }

    private void Monitor_VolumeChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (IsActive) Render();
    });

    private void Monitor_DefaultDeviceChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _monitor is null) return;
        _monitor.BindDefaultDevice();
        AppLog.Info($"Default audio device: {_monitor.DeviceName}");
        Render();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    });

    private void Monitor_MicVolumeChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (IsActive) RenderMic();
    });

    private void Monitor_DefaultMicChanged() => Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
    {
        if (!IsActive || _monitor is null) return;
        _monitor.BindDefaultMicDevice();
        AppLog.Info($"Default mic device: {_monitor.MicDeviceName}");
        RenderMic();
        if (_flyoutContainer is not null) PopulateFlyout(_flyoutContainer);
    });

    private void RenderMic()
    {
        if (_monitor is null || _flyoutMicSlider is null || _flyoutMicLevelText is null || _flyoutMicIcon is null) return;
        var hasMic = _monitor.HasMicDevice;
        var level = hasMic ? (int)Math.Round(_monitor.MicVolume * 100) : 0;
        var muted = _monitor.MicMuted;
        _updatingMic = true;
        _flyoutMicSlider.Value = level;
        _flyoutMicLevelText.Text = muted ? "—" : UiText.Format("Percent", level);
        _flyoutMicIcon.Content = FindResource(muted ? "IconMicMute" : "IconMic");
        _updatingMic = false;
    }

    private void Render()
    {
        var hasDevice = _monitor?.HasDevice == true;
        var lang = UiText.Current.Language;
        if (!hasDevice)
        {
            if (_lastHasDevice == false && _lastLanguage == lang) return;
            _lastHasDevice = false;
            _lastLanguage = lang;
            _lastLevel = -1;
            MuteButton.IsEnabled = false;
            if (_flyoutSlider is not null) _flyoutSlider.IsEnabled = false;
            if (_flyoutLevelText is not null) _flyoutLevelText.Text = string.Empty;
            if (_flyoutIcon is not null) _flyoutIcon.Content = FindResource("IconVolumeMute");
            DeviceText.Text = UiText.Get("NoAudioDevice");
            Card.ToolTip = DeviceText.Text;
            LevelText.Text = string.Empty;
            SetVolumeIcon("IconVolumeMute");
            return;
        }

        var level = (int)Math.Round(_monitor!.Volume * 100);
        var muted = _monitor.Muted;
        var deviceName = _monitor.DeviceName ?? UiText.Get("Volume");
        if (_lastHasDevice == true && _lastLevel == level && _lastMuted == muted
            && _lastDeviceName == deviceName && _lastLanguage == lang)
        {
            return;
        }

        _lastHasDevice = true;
        _lastLevel = level;
        _lastMuted = muted;
        _lastDeviceName = deviceName;
        _lastLanguage = lang;

        MuteButton.IsEnabled = true;
        if (_flyoutSlider is not null) _flyoutSlider.IsEnabled = true;
        DeviceText.Text = deviceName;
        Card.ToolTip = $"{deviceName} ({level}%)";
        LevelText.Text = muted ? "—" : UiText.Format("Percent", level);

        var isHeadphone = deviceName.Contains("headphone", StringComparison.OrdinalIgnoreCase)
            || deviceName.Contains("kulaklık", StringComparison.OrdinalIgnoreCase)
            || deviceName.Contains("headset", StringComparison.OrdinalIgnoreCase)
            || deviceName.Contains("buds", StringComparison.OrdinalIgnoreCase)
            || deviceName.Contains("airpod", StringComparison.OrdinalIgnoreCase);

        var iconKey = muted || level == 0
            ? "IconVolumeMute"
            : isHeadphone
                ? "IconHeadphones"
                : level < 50
                    ? "IconVolumeLow"
                    : "IconVolume";

        SetVolumeIcon(iconKey);

        _updating = true;
        if (_flyoutSlider is not null) _flyoutSlider.Value = level;
        if (_flyoutLevelText is not null) _flyoutLevelText.Text = LevelText.Text;
        if (_flyoutIcon is not null)
        {
            var flyoutIconKey = muted || level == 0 ? "IconVolumeMute" : level < 50 ? "IconVolumeLow" : "IconVolume";
            _flyoutIcon.Content = FindResource(flyoutIconKey);
        }
        _updating = false;
    }

    private void SetVolumeIcon(string key)
    {
        if (_lastIconKey == key) return;
        _lastIconKey = key;
        VolumeIcon.Content = FindResource(key);
    }

    private void MuteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_monitor is null) return;
        if (_monitor.ToggleMute()) Render();
        else Report(UiText.Get("ErrorVolumeChange"));
    }

    private void Card_MouseLeftButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is DependencyObject dep && IsDescendantOf(dep, MuteButton))
        {
            return;
        }

        _flyout.Toggle(Card, BuildFlyoutContent, animate: true, focusable: false);
        e.Handled = true;
    }

    private void Card_MouseWheel(object sender, MouseWheelEventArgs e)
    {
        if (_monitor?.HasDevice != true) return;
        if (_monitor.SetVolume(Math.Clamp(_monitor.Volume + Math.Sign(e.Delta) * 0.02f, 0f, 1f))) Render();
        else Report(UiText.Get("ErrorVolumeChange"));
        e.Handled = true;
    }

    private static bool IsDescendantOf(DependencyObject node, DependencyObject target)
    {
        var current = node;
        while (current is not null)
        {
            if (ReferenceEquals(current, target)) return true;
            current = current is Visual || current is System.Windows.Media.Media3D.Visual3D
                ? VisualTreeHelper.GetParent(current)
                : LogicalTreeHelper.GetParent(current);
        }
        return false;
    }
}
