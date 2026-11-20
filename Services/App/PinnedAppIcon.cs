using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Media.Imaging;
using System.Xml;
using System.Xml.Linq;

namespace OpenTaskbar.Services;

/// <summary>
/// Icons for pinned apps, which have no window to ask. The pin shortcut's icon comes first: web apps pin
/// <c>chrome_proxy.exe</c> and keep their own icon there. Store apps such as <c>WhatsApp.Root.exe</c> have no icon
/// resource, so their Start menu image is used, as for their windows. The executable's file icon comes last.
/// </summary>
internal static class PinnedAppIcon
{
    private const string WindowsAppsSegment = @"\WindowsApps\";

    public static BitmapSource? Load(string executablePath, string? iconLocation)
    {
        try
        {
            if (ParseIconLocation(iconLocation) is { } location
                && !string.Equals(location.Path, executablePath, StringComparison.OrdinalIgnoreCase)
                && ApplicationIconProvider.GetForResource(location.Path, location.Index) is { } shortcutIcon)
            {
                return shortcutIcon;
            }

            if (PackagedAppId(executablePath) is { } appId
                && ApplicationIconProvider.GetForShellItem(@"shell:AppsFolder\" + appId,
                    ApplicationIconProvider.PreferredIconPixels, iconOnly: true) is { } appIcon)
            {
                return appIcon;
            }
        }
        catch (Exception ex) when (ex is ExternalException or ArgumentException or InvalidOperationException
            or UnauthorizedAccessException or IOException or XmlException)
        {
            AppLog.Warning($"Pinned app icon lookup failed: {ex.Message}");
        }

        return ApplicationIconProvider.GetForFile(executablePath);
    }

    /// <summary>The file and index in a shortcut's IconLocation ("path,index"); null when it names no file.</summary>
    internal static (string Path, int Index)? ParseIconLocation(string? iconLocation)
    {
        if (string.IsNullOrWhiteSpace(iconLocation)) return null;
        var comma = iconLocation.LastIndexOf(',');
        var path = (comma < 0 ? iconLocation : iconLocation[..comma]).Trim().Trim('"');
        var index = comma >= 0 && int.TryParse(iconLocation[(comma + 1)..].Trim(), out var parsed) ? parsed : 0;
        path = Environment.ExpandEnvironmentVariables(path);
        return path.Length == 0 ? null : (path, index);
    }

    /// <summary>
    /// The AppUserModelID of a packaged executable: the package family from its install folder, and the application
    /// whose Executable is this file in the package manifest. Null outside WindowsApps or without a match.
    /// </summary>
    internal static string? PackagedAppId(string executablePath)
    {
        var start = executablePath.IndexOf(WindowsAppsSegment, StringComparison.OrdinalIgnoreCase);
        if (start < 0) return null;
        var packageStart = start + WindowsAppsSegment.Length;
        var packageEnd = executablePath.IndexOf('\\', packageStart);
        if (packageEnd < 0) return null;

        // A package full name is Name_Version_Architecture_ResourceId_PublisherId; the family keeps the ends.
        var parts = executablePath[packageStart..packageEnd].Split('_');
        if (parts.Length < 5) return null;
        var manifest = Path.Combine(executablePath[..packageEnd], "AppxManifest.xml");
        if (!File.Exists(manifest)) return null;

        var relative = executablePath[(packageEnd + 1)..];
        var application = XDocument.Load(manifest).Descendants()
            .FirstOrDefault(element => element.Name.LocalName == "Application"
                && string.Equals(((string?)element.Attribute("Executable"))?.Replace('/', '\\'), relative,
                    StringComparison.OrdinalIgnoreCase));
        return application?.Attribute("Id")?.Value is { Length: > 0 } id ? $"{parts[0]}_{parts[^1]}!{id}" : null;
    }
}
