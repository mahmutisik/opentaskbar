using System.Windows;
using System.Windows.Media;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public static class ThemeManager
{
    // Dock and menu glass take the surface opacity, so a theme pack replaces their source instead of the material.
    private static readonly string[] MaterialKeys = ["DockMaterial", "MenuMaterial"];
    private static ResourceDictionary? _palette;
    private static readonly ResourceDictionary Materials = new();
    // Theme pack brushes; merged last so they win over the palette.
    private static readonly ResourceDictionary Overrides = new();
    private static readonly Dictionary<string, Brush> MaterialSources = new(StringComparer.Ordinal);
    private static double _opacity = 0.9;
    /// <summary>Share of the surface opacity the dock tint keeps; lower while a blurred backdrop carries the glass.</summary>
    private static double _dockGlassScale = 1.0;

    /// <summary>With a blurred backdrop behind the dock, its own tint is lighter so the two do not stack into a dark slab.</summary>
    public static void SetDockGlass(bool blurred)
    {
        _dockGlassScale = blurred ? 0.5 : 1.0;
        if (_palette is not null) SetSurfaceOpacity(_opacity);
    }

    public static void Apply(AppTheme theme, double opacity = 0.9)
    {
        var source = $"/OpenTaskbar;component/Design/Palettes/{theme}.xaml";
        var dictionaries = Application.Current.Resources.MergedDictionaries;
        if (_palette?.Source?.OriginalString != source)
        {
            var next = new ResourceDictionary { Source = new Uri(source, UriKind.Relative) };
            if (_palette is not null) dictionaries.Remove(_palette);
            dictionaries.Add(next);
            _palette = next;
        }
        dictionaries.Remove(Materials);
        dictionaries.Remove(Overrides);
        dictionaries.Add(Materials);
        dictionaries.Add(Overrides);
        SetSurfaceOpacity(opacity);
    }

    /// <summary>True for the dictionaries this class owns, which theme packs must not treat as token definitions.</summary>
    internal static bool IsManagedDictionary(ResourceDictionary dictionary) =>
        ReferenceEquals(dictionary, _palette) || ReferenceEquals(dictionary, Materials) || ReferenceEquals(dictionary, Overrides);

    /// <summary>Replaces the theme pack's brush and color overrides; an empty map restores the palette.</summary>
    public static void SetOverrides(IReadOnlyDictionary<string, object> values)
    {
        Overrides.Clear();
        MaterialSources.Clear();
        foreach (var (key, value) in values)
        {
            if (MaterialKeys.Contains(key) && value is Brush brush) MaterialSources[key] = brush;
            else Overrides[key] = value;
        }

        if (_palette is not null) SetSurfaceOpacity(_opacity);
    }

    public static void SetSurfaceOpacity(double opacity)
    {
        if (!double.IsFinite(opacity) || opacity is < 0.3 or > 1) throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        _opacity = opacity;
        var alpha = (byte)Math.Round(opacity * 255);
        var settingsSource = Overrides.Contains("SettingsBackground") ? Overrides["SettingsBackground"] : _palette["SettingsBackground"];
        Materials["SettingsGlassMaterial"] = WithAlpha((Brush)settingsSource, alpha);
        foreach (var key in MaterialKeys)
        {
            var keyAlpha = key == "DockMaterial" ? (byte)Math.Round(opacity * _dockGlassScale * 255) : alpha;
            Materials[key] = WithAlpha(MaterialSources.TryGetValue(key, out var custom) ? custom : (Brush)_palette[key], keyAlpha);
        }
    }

    private static Brush WithAlpha(Brush source, byte alpha)
    {
        var brush = source.Clone();
        switch (brush)
        {
            case SolidColorBrush solid:
                solid.Color = Color.FromArgb(alpha, solid.Color.R, solid.Color.G, solid.Color.B);
                break;
            case GradientBrush gradient:
                foreach (var stop in gradient.GradientStops)
                {
                    stop.Color = Color.FromArgb(alpha, stop.Color.R, stop.Color.G, stop.Color.B);
                }
                break;
        }

        brush.Freeze();
        return brush;
    }
}
