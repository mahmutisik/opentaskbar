using System.Windows;
using System.Windows.Controls;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Colour-coded widget cards: a soft diagonal tint with a matching rim, the same treatment as the clock's time-of-day
/// card. The brushes are <c>WidgetTint{Name}Fill</c> and <c>WidgetTint{Name}Stroke</c> in Widgets.xaml; a card
/// without a tint keeps the neutral glass card of its style.
/// </summary>
internal static class WidgetTint
{
    public const string Teal = "Teal";
    public const string Sky = "Sky";
    public const string Coral = "Coral";
    public const string Violet = "Violet";
    public const string Amber = "Amber";
    public const string Slate = "Slate";
    public const string Red = "Red";
    public const string Indigo = "Indigo";

    public static void Apply(Control card, string? tint) => Set(card, Control.BackgroundProperty, Control.BorderBrushProperty, tint);

    public static void Apply(Border card, string? tint) => Set(card, Border.BackgroundProperty, Border.BorderBrushProperty, tint);

    /// <summary>A weather card takes the colour of the sky it describes.</summary>
    public static string ForWeatherIcon(string iconKey) => iconKey switch
    {
        "IconSun" or "IconCloudSun" => Amber,
        "IconCloudRain" or "IconSnow" => Sky,
        "IconCloudLightning" => Violet,
        _ => Slate
    };

    /// <summary>Each AI provider's card carries a colour close to its brand; the combined summary card stays neutral.</summary>
    public static string? ForAiProvider(Models.AiProviders provider) => provider switch
    {
        Models.AiProviders.Claude => Coral,
        Models.AiProviders.Codex => Teal,
        Models.AiProviders.Antigravity => Sky,
        Models.AiProviders.OpenCode => Amber,
        Models.AiProviders.Copilot => Violet,
        Models.AiProviders.DeepSeek => Indigo,
        Models.AiProviders.OpenRouter => Slate,
        _ => null
    };

    private static void Set(FrameworkElement card, DependencyProperty fill, DependencyProperty stroke, string? tint)
    {
        if (tint is null)
        {
            card.ClearValue(fill);
            card.ClearValue(stroke);
            return;
        }

        card.SetResourceReference(fill, $"WidgetTint{tint}Fill");
        card.SetResourceReference(stroke, $"WidgetTint{tint}Stroke");
    }
}
