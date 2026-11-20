using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

/// <summary>Current temperature and conditions; the flyout shows a short forecast.</summary>
public sealed class WeatherWidget : DockWidget
{
    private static readonly TimeSpan RefreshInterval = TimeSpan.FromMinutes(30);
    private static readonly TimeSpan RetryInterval = TimeSpan.FromMinutes(2);
    private readonly ContentControl _icon;
    private readonly TextBlock _temperature;
    private readonly Button _card;
    private readonly DockFlyout _flyout = new();
    private WeatherReport? _report;
    private string? _error;
    private DateTime _loadedAt = DateTime.MinValue;
    private string? _loadedFor;
    private string? _loadingFor;
    private CancellationTokenSource? _loading;

    public WeatherWidget()
    {
        _icon = new ContentControl { Width = 18, Height = 18, Margin = new Thickness(0, 0, 6, 0) };
        _icon.SetResourceReference(StyleProperty, "WidgetIcon");
        _icon.SetResourceReference(ForegroundProperty, "TextPrimary");
        _temperature = new TextBlock { FontSize = 15 };
        _temperature.SetResourceReference(StyleProperty, "WidgetText");
        var content = new StackPanel { Orientation = Orientation.Horizontal };
        content.Children.Add(_icon);
        content.Children.Add(_temperature);
        _card = new Button { Content = content, Padding = new Thickness(10, 0, 12, 0) };
        _card.SetResourceReference(StyleProperty, "WidgetCardButton");
        _card.Click += (_, _) =>
        {
            if (!IsHoverFlyoutOpen) _flyout.Toggle(_card, BuildFlyout);
        };
        Content = _card;
        HasData = false;
    }

    protected override void OnLanguageChanged() => Render();

    private string LocationKey => $"{Settings.WeatherCity.Trim()}|{Settings.Language}";

    protected override void OnActivated()
    {
        UiText.Current.PropertyChanged += UiText_PropertyChanged;
        _ = LoadAsync();
    }

    protected override void OnDeactivated()
    {
        UiText.Current.PropertyChanged -= UiText_PropertyChanged;
        _loading?.Cancel();
        _flyout.Close();
    }

    private void UiText_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e) => Render();

    protected override void OnSettingsChanged()
    {
        if (IsActive && _loadedFor != LocationKey && _loadingFor != LocationKey) _ = LoadAsync();
    }

    protected override void OnTick(DateTime now)
    {
        if (DateTime.UtcNow - _loadedAt > (_report is null ? RetryInterval : RefreshInterval) && _loading is null) _ = LoadAsync();
    }

    private async Task LoadAsync()
    {
        var key = LocationKey;
        if (_loading is not null && _loadingFor == key) return;
        _loading?.Cancel();
        var loading = new CancellationTokenSource();
        _loading = loading;
        _loadingFor = key;
        _loadedAt = DateTime.UtcNow;
        if (_loadedFor != key)
        {
            _report = null;
            _error = null;
            Render();
        }
        try
        {
            var (report, error) = await WeatherService.LoadAsync(Settings.WeatherCity, Settings.Language, loading.Token);
            if (!ReferenceEquals(_loading, loading) || !IsActive || LocationKey != key) return;
            if (report is not null || _loadedFor != key) _report = report;
            if (error is not null && _loadedFor != key && !string.IsNullOrWhiteSpace(Settings.WeatherCity)) Report(error);
            _error = _report is null ? error : null;
            _loadedFor = key;
            Render();
        }
        finally
        {
            if (ReferenceEquals(_loading, loading))
            {
                _loading = null;
                _loadingFor = null;
            }
            loading.Dispose();
        }
    }

    private void Render()
    {
        // A failed load stays visible with its cause instead of silently hiding the widget.
        HasData = _report is not null || _error is not null;
        if (_report is not { } report)
        {
            _icon.Content = null;
            WidgetTint.Apply(_card, null);
            _temperature.Text = UiText.Get("WeatherUnavailable");
            System.Windows.Automation.AutomationProperties.SetName(_card, $"{UiText.Get("Weather")}: {_error}");
            return;
        }
        var (icon, text) = WeatherService.Describe(report.Code);
        _icon.Content = FindResource(icon);
        WidgetTint.Apply(_card, WidgetTint.ForWeatherIcon(icon));
        _temperature.Text = FormatDegrees(report.Temperature);
        var condition = string.IsNullOrEmpty(report.Place) ? UiText.Get(text) : $"{report.Place} · {UiText.Get(text)}";
        System.Windows.Automation.AutomationProperties.SetName(_card, $"{UiText.Get("Weather")}: {_temperature.Text}, {condition}");
    }

    private string FormatDegrees(double value) => $"{Math.Round(value).ToString(Settings.Language.Culture)}°";

    public override UIElement? BuildFullHoverFlyoutContent() => BuildFlyout();

    private UIElement BuildFlyout()
    {
        if (_report is not { } report)
        {
            var emptyRoot = new StackPanel { Width = 280 };
            emptyRoot.Children.Add(DockFlyout.Heading(UiText.Get("Weather")));
            emptyRoot.Children.Add(DockFlyout.Caption(_error ?? UiText.Get("WeatherUnavailable")));
            return emptyRoot;
        }

        var root = new StackPanel { Width = 280 };
        var (icon, text) = WeatherService.Describe(report.Code);

        // Header: Place name & condition description
        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 6) };
        var conditionText = new TextBlock
        {
            Text = UiText.Get(text),
            FontSize = 12,
            VerticalAlignment = VerticalAlignment.Center
        };
        conditionText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        DockPanel.SetDock(conditionText, Dock.Right);
        header.Children.Add(conditionText);

        var placeHeading = new TextBlock
        {
            Text = string.IsNullOrEmpty(report.Place) ? UiText.Get("Weather") : report.Place,
            FontSize = 14,
            FontWeight = FontWeights.SemiBold,
            VerticalAlignment = VerticalAlignment.Center
        };
        placeHeading.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        header.Children.Add(placeHeading);
        root.Children.Add(header);

        // Current weather row
        var now = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 2, 0, 8) };
        var bigIcon = new ContentControl
        {
            Content = FindResource(icon),
            Width = 38,
            Height = 38,
            Margin = new Thickness(0, 0, 12, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        bigIcon.SetResourceReference(StyleProperty, "WidgetIcon");
        bigIcon.SetResourceReference(ForegroundProperty, "TextPrimary");

        var summary = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var tempText = new TextBlock
        {
            Text = FormatDegrees(report.Temperature),
            FontSize = 28,
            FontWeight = FontWeights.Light,
            Margin = new Thickness(0, 0, 0, 1)
        };
        tempText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        summary.Children.Add(tempText);

        string todayRange = string.Empty;
        if (report.Days.Count > 0)
        {
            todayRange = $" · {FormatDegrees(report.Days[0].Max)} / {FormatDegrees(report.Days[0].Min)}";
        }
        var feelsText = new TextBlock
        {
            Text = $"{UiText.Format("WeatherFeelsLike", FormatDegrees(report.ApparentTemperature))}{todayRange}",
            FontSize = 11.5
        };
        feelsText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        summary.Children.Add(feelsText);

        now.Children.Add(bigIcon);
        now.Children.Add(summary);
        root.Children.Add(now);

        // Micro metrics cards: Humidity, Wind
        var metricsGrid = new UniformGrid { Columns = 2, Margin = new Thickness(0, 0, 0, 8) };

        var humidityCard = new Border
        {
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(8, 6, 8, 6),
            Margin = new Thickness(0, 0, 3, 0)
        };
        humidityCard.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
        var humidityPanel = new StackPanel();
        var humLabel = new TextBlock { Text = UiText.Get("WeatherHumidity"), FontSize = 10.5 };
        humLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        var humVal = new TextBlock { Text = (report.Humidity / 100).ToString("P0", Settings.Language.Culture), FontSize = 12.5, FontWeight = FontWeights.SemiBold };
        humVal.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        humidityPanel.Children.Add(humLabel);
        humidityPanel.Children.Add(humVal);
        humidityCard.Child = humidityPanel;
        metricsGrid.Children.Add(humidityCard);

        var windCard = new Border
        {
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(8, 6, 8, 6),
            Margin = new Thickness(3, 0, 0, 0)
        };
        windCard.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
        var windPanel = new StackPanel();
        var windLabel = new TextBlock { Text = UiText.Get("WeatherWind"), FontSize = 10.5 };
        windLabel.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        var windVal = new TextBlock { Text = UiText.Format("WeatherWindSpeed", Math.Round(report.WindSpeed).ToString(Settings.Language.Culture)), FontSize = 12.5, FontWeight = FontWeights.SemiBold };
        windVal.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        windPanel.Children.Add(windLabel);
        windPanel.Children.Add(windVal);
        windCard.Child = windPanel;
        metricsGrid.Children.Add(windCard);

        root.Children.Add(metricsGrid);

        // Divider
        var divider = new Border
        {
            Height = 1,
            Margin = new Thickness(0, 2, 0, 8)
        };
        divider.SetResourceReference(Border.BackgroundProperty, "MenuDivider");
        root.Children.Add(divider);

        // Next 3 days forecast heading
        var forecastHeading = new TextBlock
        {
            Text = UiText.Get("WeatherForecast3Days"),
            FontSize = 11.5,
            FontWeight = FontWeights.SemiBold,
            Margin = new Thickness(0, 0, 0, 6)
        };
        forecastHeading.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        root.Children.Add(forecastHeading);

        // Next 3 days forecast cards
        var culture = Settings.Language.Culture;
        var forecastDays = report.Days.Skip(1).Take(3).ToList();
        var daysGrid = new UniformGrid { Columns = Math.Max(1, forecastDays.Count), Margin = new Thickness(0, 0, 0, 4) };

        for (int i = 0; i < forecastDays.Count; i++)
        {
            var day = forecastDays[i];
            var dayCard = new Border
            {
                CornerRadius = new CornerRadius(8),
                Padding = new Thickness(4, 6, 4, 6),
                Margin = new Thickness(i == 0 ? 0 : 2, 0, i == forecastDays.Count - 1 ? 0 : 2, 0)
            };
            dayCard.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");

            var column = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center };
            var dayTitle = new TextBlock
            {
                Text = day.Date.ToString("ddd", culture).ToUpper(culture),
                FontSize = 10.5,
                FontWeight = FontWeights.SemiBold,
                HorizontalAlignment = HorizontalAlignment.Center
            };
            dayTitle.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            column.Children.Add(dayTitle);

            var (dayIconKey, dayTextKey) = WeatherService.Describe(day.Code);
            var dayIcon = new ContentControl
            {
                Content = FindResource(dayIconKey),
                Width = 20,
                Height = 20,
                Margin = new Thickness(0, 4, 0, 3),
                HorizontalAlignment = HorizontalAlignment.Center
            };
            dayIcon.SetResourceReference(StyleProperty, "WidgetIcon");
            dayIcon.SetResourceReference(ForegroundProperty, "TextPrimary");
            column.Children.Add(dayIcon);

            var dayDesc = new TextBlock
            {
                Text = UiText.Get(dayTextKey),
                FontSize = 10,
                HorizontalAlignment = HorizontalAlignment.Center,
                Margin = new Thickness(0, 0, 0, 3),
                TextTrimming = TextTrimming.CharacterEllipsis
            };
            dayDesc.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            column.Children.Add(dayDesc);

            var tempPanel = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Center };
            var maxText = new TextBlock
            {
                Text = FormatDegrees(day.Max),
                FontSize = 11.5,
                FontWeight = FontWeights.SemiBold,
                Margin = new Thickness(0, 0, 3, 0)
            };
            maxText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
            var minText = new TextBlock
            {
                Text = FormatDegrees(day.Min),
                FontSize = 11.5
            };
            minText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
            tempPanel.Children.Add(maxText);
            tempPanel.Children.Add(minText);
            column.Children.Add(tempPanel);

            dayCard.Child = column;
            daysGrid.Children.Add(dayCard);
        }

        root.Children.Add(daysGrid);

        var footer = new TextBlock
        {
            Text = "Open-Meteo",
            FontSize = 9.5,
            HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 4, 0, 0)
        };
        footer.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        root.Children.Add(footer);

        return root;
    }
}
