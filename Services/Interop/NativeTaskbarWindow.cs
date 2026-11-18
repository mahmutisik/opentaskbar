using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Finds Explorer's taskbar window. When OpenTaskbar hosts the notification area it owns a second, invisible
/// <c>Shell_TrayWnd</c> that is kept topmost so apps register their tray icons with it; a plain
/// <c>FindWindow("Shell_TrayWnd")</c> would then return that window instead of the real taskbar.
/// </summary>
internal static class NativeTaskbarWindow
{
    private const string TrayWindowClass = "Shell_TrayWnd";

    /// <summary>Explorer's primary taskbar, skipping any <c>Shell_TrayWnd</c> owned by this process.</summary>
    public static nint Find()
    {
        nint candidate = 0;
        while ((candidate = FindWindowEx(0, candidate, TrayWindowClass, null)) != 0)
        {
            GetWindowThreadProcessId(candidate, out var processId);
            if (processId != Environment.ProcessId) return candidate;
        }
        return 0;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern nint FindWindowEx(nint parent, nint childAfter, string? className, string? windowName);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);
}
