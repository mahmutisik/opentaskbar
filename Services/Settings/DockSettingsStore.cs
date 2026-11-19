using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class DockSettingsStore
{
    private const string RemovedMemoryWidgetId = "Memory";
    private static readonly JsonSerializerOptions JsonOptions = CreateJsonOptions();

    private readonly string _path;
    private readonly AppLanguage _defaultLanguage;

    public DockSettingsStore(string? path = null, CultureInfo? defaultCulture = null)
    {
        _defaultLanguage = LanguageCatalog.FromCulture(defaultCulture ?? CultureInfo.CurrentUICulture);
        _path = path ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "OpenTaskbar",
            "settings.json");
    }

    public (DockSettings Settings, string? Error) Load()
    {
        if (!File.Exists(_path))
        {
            return (CreateDefaultSettings(), null);
        }

        try
        {
            using var document = JsonDocument.Parse(File.ReadAllText(_path));
            var settings = document.RootElement.Deserialize<DockSettings>(JsonOptions);
            // Older files without a language follow the same system-language rule as a first launch.
            if (settings is not null && !document.RootElement.TryGetProperty(nameof(DockSettings.Language), out _))
                settings = settings with { Language = _defaultLanguage };
            // Normalize the legacy enabled-only format before enforcing the installed subset invariant.
            if (settings is { InstalledWidgets: null })
                settings = settings with { InstalledWidgets = settings.Widgets ?? Array.Empty<string>() };
            // The standalone memory widget was removed in favor of System Meters; drop stale identifiers.
            if (settings is not null)
                settings = settings with
                {
                    Widgets = settings.Widgets?.Where(id => id != RemovedMemoryWidgetId).ToArray(),
                    InstalledWidgets = settings.InstalledWidgets?.Where(id => id != RemovedMemoryWidgetId).ToArray()
                };
            if (settings is null || !IsValid(settings))
            {
                return (CreateDefaultSettings(), "ErrorDockSettingsInvalid");
            }

            return (settings, null);
        }
        catch (JsonException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsRead");
        }
        catch (IOException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsDisk");
        }
        catch (UnauthorizedAccessException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsAccess");
        }
    }

    private DockSettings CreateDefaultSettings() => new(Language: _defaultLanguage);

    public string? Save(DockSettings settings)
    {
        if (!IsValid(settings))
            return "ErrorSettingsValuesInvalid";
        try
        {
            var directory = Path.GetDirectoryName(_path);
            if (directory is null)
            {
                return "ErrorDockSettingsPath";
            }

            Directory.CreateDirectory(directory);
            var temporaryPath = _path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                File.WriteAllText(temporaryPath, JsonSerializer.Serialize(settings, JsonOptions));
                File.Move(temporaryPath, _path, overwrite: true);
            }
            finally
            {
                if (File.Exists(temporaryPath)) File.Delete(temporaryPath);
            }
            return null;
        }
        catch (IOException)
        {
            return "ErrorDockSettingsSave";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorDockSettingsWriteAccess";
        }
    }

    private static bool IsValid(DockSettings settings) => Enum.IsDefined(settings.Position)
        && Enum.IsDefined(settings.Theme) && LanguageCatalog.IsSupported(settings.Language)
        && Enum.IsDefined(settings.AutoHide) && Enum.IsDefined(settings.Layout) && Enum.IsDefined(settings.Size)
        // Keep these ranges identical to the Widgets tab sliders.
        && settings.FocusMinutes is >= 1 and <= 90
        && settings.FocusBreakMinutes is >= 1 and <= 30
        && settings.FocusSessions is >= 1 and <= 8
        && double.IsFinite(settings.SurfaceOpacity) && settings.SurfaceOpacity is >= 0.3 and <= 1
        && double.IsFinite(settings.DockScale) && settings.DockScale is >= 0.75 and <= 1.5
        && settings.PinOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.PinOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.PinOrder?.Count
        && settings.Widgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.Widgets?.Distinct(StringComparer.Ordinal).Count() == settings.Widgets?.Count
        && settings.InstalledWidgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.InstalledWidgets?.Distinct(StringComparer.Ordinal).Count() == settings.InstalledWidgets?.Count
        && settings.Widgets?.Any(widget => settings.InstalledWidgets?.Contains(widget, StringComparer.Ordinal) != true) != true
        && Enum.IsDefined(settings.LauncherHotkey) && Enum.IsDefined(settings.DockHotkey)
        && Enum.IsDefined(settings.StartAction) && Enum.IsDefined(settings.StartPlacement) && Enum.IsDefined(settings.Edge)
        && Enum.IsDefined(settings.DateStyle)
        && (settings.StatsMeters & ~SystemMeters.All) == 0
        && (settings.AiUsageProviders & ~AiProviders.All) == 0
        && Enum.IsDefined(settings.AiUsageRing)
        && settings.WeatherCity is { Length: <= 80 }
        && settings.StackFolders?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.WidgetOrder?.Count
        && settings.Profiles?.Any(profile => string.IsNullOrWhiteSpace(profile.Name) || profile.HiddenApps is null) != true
        && settings.Profiles?.Select(profile => profile.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.Profiles?.Count
        && (settings.DockCornerRadius is null || settings.DockCornerRadius is >= 0 and <= 48)
        && (settings.DockBottomGap is null || settings.DockBottomGap is >= 0 and <= 48);

    public static bool Validate(DockSettings settings) => IsValid(settings);

    public static JsonSerializerOptions SerializerOptions => JsonOptions;

    private static JsonSerializerOptions CreateJsonOptions()
    {
        var options = new JsonSerializerOptions { WriteIndented = true };
        options.Converters.Add(new AppLanguageConverter());
        options.Converters.Add(new JsonStringEnumConverter());
        return options;
    }

    private sealed class AppLanguageConverter : JsonConverter<AppLanguage>
    {
        public override AppLanguage Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
        {
            if (reader.TokenType == JsonTokenType.Number && reader.TryGetInt32(out var legacyValue))
            {
                return legacyValue switch
                {
                    0 => AppLanguage.Turkish,
                    1 => AppLanguage.English,
                    2 => AppLanguage.Japanese,
                    _ => throw new JsonException("Legacy language value is not supported.")
                };
            }
            if (reader.TokenType != JsonTokenType.String) throw new JsonException("Language must be a culture name.");
            var value = reader.GetString();
            if (string.Equals(value, "Turkish", StringComparison.OrdinalIgnoreCase)) return AppLanguage.Turkish;
            if (string.Equals(value, "English", StringComparison.OrdinalIgnoreCase)) return AppLanguage.English;
            if (string.Equals(value, "Japanese", StringComparison.OrdinalIgnoreCase)) return AppLanguage.Japanese;
            if (value is not null)
            {
                try
                {
                    var language = new AppLanguage(value);
                    if (LanguageCatalog.IsSupported(language)) return language;
                }
                catch (CultureNotFoundException)
                {
                    throw new JsonException("Language culture is invalid.");
                }
            }

            throw new JsonException("Language is not supported.");
        }

        public override void Write(Utf8JsonWriter writer, AppLanguage value, JsonSerializerOptions options) =>
            writer.WriteStringValue(value.CultureName);
    }
}
