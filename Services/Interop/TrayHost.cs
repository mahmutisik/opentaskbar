using System.Collections.ObjectModel;
using System.Runtime.InteropServices;
using System.Windows.Input;
using ManagedShell.WindowsTray;

namespace OpenTaskbar.Services;

/// <summary>
/// PROTOTYPE: hosts the notification area so background apps' tray icons, and their own menus, are reachable while
/// Explorer's taskbar is hidden. Built on ManagedShell (Apache-2.0, the library RetroBar uses): it owns an invisible
/// topmost <c>Shell_TrayWnd</c>, so <c>Shell_NotifyIcon</c> calls reach it, and broadcasts <c>TaskbarCreated</c> so
/// running apps re-register their icons. Messages it does not handle are forwarded to Explorer's taskbar. On
/// shutdown it broadcasts <c>TaskbarCreated</c> again so the icons return to Explorer.
/// </summary>
internal sealed class TrayHost : IDisposable
{
    /// <summary>Shell-owned system icons (volume, network, power, ...); the dock has its own widgets for these.</summary>
    private static readonly HashSet<Guid> SystemIconGuids =
    [
        new(NotificationArea.VOLUME_GUID), new(NotificationArea.NETWORK_GUID), new(NotificationArea.POWER_GUID),
        new(NotificationArea.HEALTH_GUID), new(NotificationArea.MEETNOW_GUID), new(NotificationArea.LOCATION_GUID),
        new(NotificationArea.MICROPHONE_GUID)
    ];

    private TrayService? _trayService;
    private NotificationArea? _area;

    public static TrayHost? Current { get; private set; }

    public bool IsRunning => _area is { IsFailed: false };

    public ObservableCollection<NotifyIcon> Icons => _area?.TrayIcons ?? [];

    /// <summary>Starts hosting on the UI thread; a failure leaves the registry-based tray list in place.</summary>
    public static TrayHost? Start()
    {
        if (Current is not null) return Current;
        var host = new TrayHost();
        try
        {
            host._trayService = new TrayService();
            host._area = new NotificationArea(host._trayService, new ExplorerTrayService());
            host._area.Initialize();
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException or ExternalException or TypeInitializationException)
        {
            AppLog.Warning($"Tray host failed to start: {ex.Message}");
        }

        if (!host.IsRunning)
        {
            AppLog.Warning("Tray host unavailable; using the registry tray list.");
            host.Dispose();
            return null;
        }

        AppLog.Info("Tray host started.");
        return Current = host;
    }

    /// <summary>Visible, app-owned icons in their registration order.</summary>
    public IReadOnlyList<NotifyIcon> AppIcons() =>
        Icons.Where(icon => !icon.IsHidden && !SystemIconGuids.Contains(icon.GUID) && icon.HWnd != 0).ToList();

    /// <summary>Sends the click the native taskbar would send, so the app shows its own window or menu.</summary>
    public static void Click(NotifyIcon icon, MouseButton button)
    {
        GetCursorPos(out var cursor);
        var position = (uint)((cursor.Y << 16) | (cursor.X & 0xFFFF));
        var doubleClickTime = (int)GetDoubleClickTime();
        icon.IconMouseDown(button, position, doubleClickTime);
        icon.IconMouseUp(button, position, doubleClickTime);
    }

    public void Dispose()
    {
        try
        {
            _area?.Dispose();
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException or ExternalException)
        {
            AppLog.Warning($"Tray host shutdown: {ex.Message}");
        }

        _area = null;
        _trayService = null;
        if (ReferenceEquals(Current, this)) Current = null;
    }

    /// <summary>
    /// Asks every running app to register its tray icons again with whichever <c>Shell_TrayWnd</c> is current.
    /// Used after an unclean exit, when icons were left registered with this process's destroyed tray window.
    /// </summary>
    public static void BroadcastTaskbarCreated()
    {
        var message = RegisterWindowMessage("TaskbarCreated");
        if (message != 0) SendNotifyMessage(new nint(0xFFFF), message, 0, 0);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint point);

    [DllImport("user32.dll")]
    private static extern uint GetDoubleClickTime();

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "RegisterWindowMessageW")]
    private static extern uint RegisterWindowMessage(string message);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SendNotifyMessage(nint windowHandle, uint message, nuint wParam, nint lParam);
}
