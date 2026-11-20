using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Widgets;

public partial class ClockWidget : DockWidget
{
    public enum TimeOfDayPeriod { Morning, Noon, Evening }

    public sealed record TimeOfDayTheme(Brush Background, Brush Border, Brush Accent, string IconKey);

    private static readonly TimeOfDayTheme MorningTheme = CreateTimeTheme(
        Color.FromArgb(0x38, 0xF5, 0x9E, 0x0B),
        Color.FromArgb(0x10, 0xFB, 0x92, 0x3C),
        Color.FromArgb(0x48, 0xF5, 0x9E, 0x0B),
        Color.FromRgb(0xF5, 0x9E, 0x0B),
        "IconSun");

    private static readonly TimeOfDayTheme NoonTheme = CreateTimeTheme(
        Color.FromArgb(0x34, 0x38, 0xBD, 0xF8),
        Color.FromArgb(0x0E, 0x02, 0x84, 0xC7),
        Color.FromArgb(0x48, 0x38, 0xBD, 0xF8),
        Color.FromRgb(0x0E, 0xA5, 0xE9),
        "IconSun");

    private static readonly TimeOfDayTheme EveningTheme = CreateTimeTheme(
        Color.FromArgb(0x3C, 0x63, 0x66, 0xF1),
        Color.FromArgb(0x12, 0x43, 0x38, 0xCA),
        Color.FromArgb(0x4E, 0x81, 0x8C, 0xF8),
        Color.FromRgb(0xA7, 0x8B, 0xFA),
        "IconMoon");

    private static TimeOfDayTheme CreateTimeTheme(
        Color bgStart, Color bgEnd, Color borderColor, Color accentColor, string iconKey)
    {
        var bg = new LinearGradientBrush(bgStart, bgEnd, new Point(0, 0), new Point(1, 1));
        bg.Freeze();
        var border = new SolidColorBrush(borderColor);
        border.Freeze();
        var accent = new SolidColorBrush(accentColor);
        accent.Freeze();
        return new TimeOfDayTheme(bg, border, accent, iconKey);
    }

    public static TimeOfDayPeriod GetPeriod(int hour) => hour switch
    {
        >= 6 and < 11 => TimeOfDayPeriod.Morning,
        >= 11 and < 18 => TimeOfDayPeriod.Noon,
        _ => TimeOfDayPeriod.Evening
    };

    public static TimeOfDayTheme GetTheme(TimeOfDayPeriod period) => period switch
    {
        TimeOfDayPeriod.Morning => MorningTheme,
        TimeOfDayPeriod.Noon => NoonTheme,
        _ => EveningTheme
    };

    private readonly DockFlyout _calendar = new();
    private string _lastRenderKey = string.Empty;
    private TimeOfDayPeriod? _currentPeriod;
    private DateOnly _shownMonth;

    public ClockWidget() => InitializeComponent();

    private CultureInfo Culture => Settings.Language.Culture;

    protected override void OnActivated()
    {
        _lastRenderKey = string.Empty;
        _currentPeriod = null;
        OnTick(DateTime.Now);
    }

    protected override void OnSettingsChanged()
    {
        _lastRenderKey = string.Empty;
        _currentPeriod = null;
        if (IsActive) OnTick(DateTime.Now);
    }

    protected override void OnDeactivated() => _calendar.Close();

    protected override void OnTick(DateTime now)
    {
        var culture = Culture;
        var period = GetPeriod(now.Hour);
        if (_currentPeriod != period)
        {
            _currentPeriod = period;
            var theme = GetTheme(period);
            ClockButton.Background = theme.Background;
            ClockButton.BorderBrush = theme.Border;
            TimeOfDayIcon.Content = FindResource(theme.IconKey);
            TimeOfDayIcon.Foreground = theme.Accent;
            DayOfWeekText.Foreground = theme.Accent;
        }

        var timeFormat = Settings.Clock24Hour
            ? (Settings.ClockShowSeconds ? "HH:mm:ss" : "HH:mm")
            : (Settings.ClockShowSeconds ? "hh:mm:ss tt" : "hh:mm tt");
        var timeString = now.ToString(timeFormat, culture);
        var renderKey = $"{now:yyyyMMdd}|{timeString}|{Settings.DateStyle}|{Settings.Language}|{period}";
        if (renderKey != _lastRenderKey)
        {
            _lastRenderKey = renderKey;
            TimeText.Text = timeString;
            switch (Settings.DateStyle)
            {
                case ClockDateStyle.DayAndShortDate:
                    DateColumn.Visibility = Visibility.Visible;
                    DayOfWeekText.Visibility = Visibility.Visible;
                    DayOfWeekText.Text = now.ToString("dddd", culture).ToUpper(culture);
                    DateText.Text = now.ToString(culture.DateTimeFormat.MonthDayPattern.Replace("MMMM", "MMM"), culture);
                    break;
                case ClockDateStyle.ShortDateOnly:
                    DateColumn.Visibility = Visibility.Visible;
                    DayOfWeekText.Visibility = Visibility.Collapsed;
                    DateText.Text = now.ToString("MMM d", culture);
                    break;
                case ClockDateStyle.NumericDate:
                    DateColumn.Visibility = Visibility.Visible;
                    DayOfWeekText.Visibility = Visibility.Collapsed;
                    DateText.Text = now.ToString("d", culture);
                    break;
                case ClockDateStyle.Hidden:
                    DateColumn.Visibility = Visibility.Collapsed;
                    break;
            }
            ClockButton.ToolTip = now.ToString("D", culture);
        }
    }

    private void ClockButton_Click(object sender, RoutedEventArgs e) => _calendar.Toggle(ClockButton, BuildCalendarFlyout);

    public override UIElement? BuildFullHoverFlyoutContent() => BuildCalendarFlyout();

    /// <summary>The calendar flyout, opened on this month.</summary>
    private UIElement BuildCalendarFlyout()
    {
        _shownMonth = DateOnly.FromDateTime(DateTime.Today);
        _shownMonth = new DateOnly(_shownMonth.Year, _shownMonth.Month, 1);
        var root = new ContentControl();
        root.Content = BuildCalendar(root);
        return root;
    }

    /// <summary>Month view with the user's first day of week; arrows rebuild it in place.</summary>
    private UIElement BuildCalendar(ContentControl host)
    {
        var culture = Culture;
        var today = DateOnly.FromDateTime(DateTime.Today);
        var theme = GetTheme(GetPeriod(DateTime.Now.Hour));

        var root = new StackPanel { Width = 280 };
        root.Children.Add(new TextBlock
        {
            Text = DateTime.Today.ToString("D", culture), FontWeight = FontWeights.SemiBold, FontSize = 14, Margin = new Thickness(0, 0, 0, 8)
        });

        var header = new DockPanel { Margin = new Thickness(0, 0, 0, 6) };
        var previous = NavButton("IconChevronLeft", -1, host);
        var next = NavButton("IconChevronNext", 1, host);
        DockPanel.SetDock(previous, Dock.Left);
        DockPanel.SetDock(next, Dock.Right);
        header.Children.Add(previous);
        header.Children.Add(next);
        var title = _shownMonth.ToString("MMMM yyyy", culture);
        header.Children.Add(new TextBlock
        {
            Text = culture.TextInfo.ToTitleCase(title), HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center, FontWeight = FontWeights.SemiBold, FontSize = 13
        });
        root.Children.Add(header);

        var grid = new UniformGrid { Columns = 7, Margin = new Thickness(0, 0, 0, 4) };
        var firstDay = culture.DateTimeFormat.FirstDayOfWeek;
        for (var index = 0; index < 7; index++)
        {
            var day = (DayOfWeek)(((int)firstDay + index) % 7);
            var name = DockFlyout.Caption(culture.DateTimeFormat.GetShortestDayName(day));
            name.HorizontalAlignment = HorizontalAlignment.Center;
            name.Margin = new Thickness(0, 0, 0, 4);
            grid.Children.Add(name);
        }

        var offset = ((int)_shownMonth.DayOfWeek - (int)firstDay + 7) % 7;
        var start = _shownMonth.AddDays(-offset);
        for (var cell = 0; cell < 42; cell++)
        {
            var date = start.AddDays(cell);
            var label = new TextBlock
            {
                Text = date.Day.ToString(culture), HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center,
                FontSize = 12
            };
            var border = new Border { Width = 32, Height = 30, CornerRadius = new CornerRadius(15), Child = label, Margin = new Thickness(1) };
            if (date == today)
            {
                border.Background = theme.Accent;
                label.SetResourceReference(TextBlock.ForegroundProperty, "WidgetAccentText");
                label.FontWeight = FontWeights.SemiBold;
            }
            else if (date.Month != _shownMonth.Month)
            {
                label.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
                label.Opacity = 0.5;
            }

            grid.Children.Add(border);
        }

        root.Children.Add(grid);

        return root;
    }

    private Button NavButton(string icon, int months, ContentControl host)
    {
        var glyph = new ContentControl { Content = FindResource(icon), Width = 14, Height = 14 };
        glyph.SetResourceReference(StyleProperty, "WidgetIcon");
        glyph.SetResourceReference(ForegroundProperty, "TextPrimary");
        var button = new Button { Content = glyph };
        button.SetResourceReference(StyleProperty, "WidgetIconButton");
        var key = months < 0 ? "PreviousMonth" : "NextMonth";
        button.ToolTip = UiText.Get(key);
        System.Windows.Automation.AutomationProperties.SetName(button, UiText.Get(key));
        button.Click += (_, _) =>
        {
            _shownMonth = _shownMonth.AddMonths(months);
            host.Content = BuildCalendar(host);
        };
        return button;
    }
}
