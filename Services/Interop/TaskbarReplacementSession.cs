using System.Globalization;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows;
using System.Windows.Threading;

namespace OpenTaskbar.Services;

public sealed class TaskbarReplacementSession : IDisposable
{
    private const uint AbmNew = 0x00000000;
    private const uint AbmRemove = 0x00000001;
    private const uint AbmQueryPos = 0x00000002;
    private const uint AbmSetPos = 0x00000003;
    private const uint AbmGetState = 0x00000004;
    private const uint AbmGetTaskbarPos = 0x00000005;
    private const uint AbmSetState = 0x0000000A;
    private const uint AbeLeft = 0;
    private const uint AbeRight = 2;
    private const uint AbsAutoHide = 0x00000001;
    private const uint AbsAlwaysOnTop = 0x00000002;
    private const uint AbeBottom = 3;
    private const int GwlStyle = -16;
    private const long WsCaption = 0x00C00000;
    private const uint SupportedTaskbarState = AbsAutoHide | AbsAlwaysOnTop;
    private const uint AbnStateChange = 0;
    private const uint AbnPosChanged = 1;
    private const uint AbnFullScreenApp = 2;
    private const uint WmDisplayChange = 0x007E;
    private const uint WmDpiChanged = 0x02E0;
    private const uint CallbackMessage = 0x8000 + 0x52;
    private const uint MonitorDefaultToNearest = 2;
    private const uint GaRootOwner = 3;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpFrameChanged = 0x0020;
    private const uint SwpShowWindow = 0x0040;
    private const uint SwpNoOwnerZOrder = 0x0200;
    private const int SwHide = 0;
    private const int SwShowNoActivate = 4;
    private const int SwShow = 5;
    private static readonly nint HwndTopmost = new(-1);
    private const int RestoreAttempts = 4;
    private const int RestoreAttemptDelayMs = 120;

    private readonly Window _window;
    private readonly nint _handle;
    private bool _registered;
    private bool _active;
    private bool _stateChangeStarted;
    private uint _previousTaskbarState;
    private bool _disposed;
    private bool _fullScreenSuppressed;
    private double _nativeBottomTaskbarHeight;
    private bool _nativeTaskbarAtBottom = true;
    private DispatcherTimer? _hideRetryTimer;
    private int _hideRetryAttempts;
    private bool _hideRetryPending;
    private double? _cachedHeadroomDips;
    private double _shelfHeight = DockMetrics.ShelfHeight;
    private (int Coordinate, int Top, int Edge, int Reserved)? _sideReservation;
    private const double DockHeadroomDips = DockMetrics.Headroom;
    private bool _autoHideEnabled;
    private bool _positioning;
    private (int Left, int Bottom, int NativePixels, int ExtraHeight)? _reservation;
    private (int Left, int Top, int Width, int Height, bool FullScreenSuppressed)? _lastWindowPos;
    private Size _lastLayout;
    private int _stopInProgress;

    public TaskbarReplacementSession(Window window, nint handle, bool autoHideEnabled)
    {
        _window = window;
        _handle = handle;
        _autoHideEnabled = autoHideEnabled;
        AppDomain.CurrentDomain.ProcessExit += Process_Exit;
    }

    public bool IsActive => _active || _stateChangeStarted;

    // Explorer's hidden taskbar keeps reserving the work area; auto-hide releases it for the dock's hide modes and
    // for side docks, which would otherwise leave an empty strip along the bottom.
    private uint ReplacementState => (_previousTaskbarState & AbsAlwaysOnTop)
        | (_autoHideEnabled || IsVertical || !_nativeTaskbarAtBottom ? AbsAutoHide : 0);

    private Models.DockEdge _edge = Models.DockEdge.Bottom;
    private bool IsVertical => _edge != Models.DockEdge.Bottom;

    /// <summary>The screen edge the dock occupies.</summary>
    public Models.DockEdge Edge
    {
        get => _edge;
        set
        {
            if (_edge == value) return;
            _edge = value;
            _reservation = null;
            _sideReservation = null;
            _lastWindowPos = null;
            _lastLayout = default;
            if (!_active || Volatile.Read(ref _stopInProgress) != 0) return;
            if (_registered)
            {
                // Drop the old edge's reservation before claiming the new one.
                var data = CreateData();
                SHAppBarMessage(AbmRemove, ref data);
                data = CreateData();
                data.uCallbackMessage = CallbackMessage;
                _registered = SHAppBarMessage(AbmNew, ref data) != 0;
            }

            SetTaskbarState(ReplacementState, _handle);
            PositionDockWindow();
        }
    }

    private nint _targetMonitor;

    /// <summary>The monitor handle where the dock is positioned, or 0 to use nearest monitor.</summary>
    public nint TargetMonitor
    {
        get => _targetMonitor;
        set => SetTargetMonitor(value);
    }

    public void SetTargetMonitor(nint monitorHandle)
    {
        if (_targetMonitor == monitorHandle) return;
        _targetMonitor = monitorHandle;
        _reservation = null;
        _sideReservation = null;
        _lastWindowPos = null;
        _lastLayout = default;
        if (!_active || Volatile.Read(ref _stopInProgress) != 0) return;
        if (_registered)
        {
            var data = CreateData();
            SHAppBarMessage(AbmRemove, ref data);
            data = CreateData();
            data.uCallbackMessage = CallbackMessage;
            _registered = SHAppBarMessage(AbmNew, ref data) != 0;
        }

        PositionDockWindow();
    }

    private static TaskbarReplacementSession? _current;

    /// <summary>
    /// Restores the Windows taskbar from any thread. Used by crash handlers, where the UI thread may be unusable.
    /// </summary>
    public static string? EmergencyRestore() => Volatile.Read(ref _current)?.Stop();

    /// <summary>
    /// True when a previous instance left a recovery journal behind. The journal is only removed after a verified
    /// restore, so its presence always means "the native taskbar may still be hidden".
    /// </summary>
    public static bool HasPendingRecovery => File.Exists(JournalPath);

    /// <summary>
    /// Startup self-heal: a crashed or killed instance can leave the taskbar hidden. Restore it before any dock
    /// mode (including --preview and --inspect, which never replace the taskbar) is selected.
    /// </summary>
    public static string? HealPendingRecovery() => HasPendingRecovery ? ForceRestoreNativeTaskbar() : null;

    /// <summary>
    /// Forcefully restores the Windows taskbar and unhides all tray windows, even if no session is active.
    /// Safe to call from any thread and from a separate process (--restore / watchdog). The journal is deleted
    /// only after the taskbar is verified visible, so a transient Explorer restart cannot lose the recovery state.
    /// </summary>
    public static string? ForceRestoreNativeTaskbar()
    {
        var state = 0u;
        if (File.Exists(JournalPath))
        {
            try
            {
                // A damaged journal must fail safe: a visible taskbar with default state beats no taskbar.
                state = FallbackState(File.ReadAllText(JournalPath));
            }
            catch
            {
                state = 0;
            }
        }

        var error = RestoreWithVerification(state, 0);
        if (error is not null) return error;
        return DeleteJournal();
    }

    public static bool HasActiveSession => Volatile.Read(ref _current)?.IsActive == true;

    public event Action<string, bool>? OperationReported;
    public event Action<Size>? LayoutChanged;
    public event Action<bool>? FullScreenChanged;

    public void UpdateForeground(nint windowHandle)
    {
        if (!_active || Volatile.Read(ref _stopInProgress) != 0) return;
        GetWindowThreadProcessId(windowHandle, out var processId);
        if (processId == Environment.ProcessId)
        {
            SetFullScreenSuppression(false);
            return;
        }
        SetFullScreenSuppression(IsFullScreen(windowHandle));
    }

    private static bool HasPendingRecoveryFile(out string? error)
    {
        error = null;
        if (!File.Exists(JournalPath))
        {
            return false;
        }

        try
        {
            _ = ReadJournal();
            return true;
        }
        catch (InvalidDataException)
        {
            error = "ErrorTaskbarJournalInvalid";
        }
        catch (IOException)
        {
            error = "ErrorTaskbarJournalRead";
        }
        catch (UnauthorizedAccessException)
        {
            error = "ErrorTaskbarJournalAccess";
        }

        return false;
    }

    public void Start()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (_active)
        {
            return;
        }

        var pending = HasPendingRecoveryFile(out var recoveryError);
        if (recoveryError is not null)
        {
            throw new InvalidOperationException(recoveryError);
        }

        _previousTaskbarState = pending ? ReadJournal() : GetTaskbarState();
        ReadNativeTaskbarHeight();
        try
        {
            if (!pending)
            {
                WriteJournal(_previousTaskbarState);
            }
        }
        catch (IOException ex)
        {
            throw new InvalidOperationException("ErrorTaskbarJournalWrite", ex);
        }
        catch (UnauthorizedAccessException ex)
        {
            throw new InvalidOperationException("ErrorTaskbarJournalWriteAccess", ex);
        }

        try
        {
            _stateChangeStarted = true;
            Volatile.Write(ref _current, this);
            if (!SetTaskbarState(ReplacementState, _handle))
            {
                throw new InvalidOperationException("ErrorTaskbarEdgeTrigger");
            }

            if (!HideNativeTaskbar())
            {
                throw new InvalidOperationException("ErrorTaskbarHide");
            }

            var data = CreateData();
            data.uCallbackMessage = CallbackMessage;
            if (SHAppBarMessage(AbmNew, ref data) == 0)
            {
                throw new InvalidOperationException("ErrorAppBarRegister");
            }

            _registered = true;
            _reservation = null;
            _lastWindowPos = null;
            _active = true;
            PositionDockWindow(throwOnFailure: true);
            TaskbarRecoveryWatchdog.Start(Environment.ProcessId);
        }
        catch
        {
            Stop();
            if (!_active) _stateChangeStarted = false;
            throw;
        }
    }

    public void HandleTaskbarCreated()
    {
        if (!_active)
        {
            return;
        }

        _registered = false;
        _reservation = null;
        _sideReservation = null;
        _lastWindowPos = null;
        try
        {
            ReadNativeTaskbarHeight();
        }
        catch (InvalidOperationException ex)
        {
            OperationReported?.Invoke(ex.Message, true);
            return;
        }

        if (!SetTaskbarState(ReplacementState, _handle))
        {
            OperationReported?.Invoke("ErrorTaskbarAutoHideToggle", true);
            return;
        }

        var data = CreateData();
        data.uCallbackMessage = CallbackMessage;
        if (SHAppBarMessage(AbmNew, ref data) == 0)
        {
            OperationReported?.Invoke("ErrorAppBarRegister", true);
            return;
        }
        _registered = true;

        if (!HideNativeTaskbar()) RetryHideNativeTaskbar();

        PositionDockWindow();
    }

    public bool HandleMessage(uint message, nint wParam, nint lParam)
    {
        if (!_active || Volatile.Read(ref _stopInProgress) != 0)
        {
            return false;
        }

        if (message == CallbackMessage)
        {
            switch (unchecked((uint)wParam.ToInt64()))
            {
                case AbnPosChanged:
                    _lastWindowPos = null;
                    EnforceNativeTaskbarHidden();
                    PositionDockWindow();
                    return true;
                case AbnFullScreenApp:
                    if (lParam != 0)
                    {
                        UpdateForeground(GetForegroundWindow());
                    }
                    else
                    {
                        SetFullScreenSuppression(false);
                        PositionDockWindow();
                    }
                    return true;
                case AbnStateChange:
                    EnforceNativeTaskbarHidden();
                    return true;
            }
        }

        if (message == WmDisplayChange)
        {
            _lastWindowPos = null;
            _reservation = null;
            _sideReservation = null;
            EnforceNativeTaskbarHidden();
            PositionDockWindow();
        }
        else if (message == WmDpiChanged)
        {
            _lastWindowPos = null;
            _reservation = null;
            _sideReservation = null;
            PositionDockWindow();
        }

        return false;
    }

    public string? Stop()
    {
        if (Interlocked.CompareExchange(ref _stopInProgress, 1, 0) != 0)
        {
            return "ErrorTaskbarRestoreInProgress";
        }

        try
        {
            // A journal on disk is enough reason to restore: a previous Start() may have died between writing it
            // and flipping _stateChangeStarted, or Explorer may have re-hidden the tray after a partial stop.
            if (!_active && !_registered && !_stateChangeStarted && !File.Exists(JournalPath)) return null;

            // Stop enforcing hidden state before Shell calls can reenter our AppBar callback.
            _active = false;
            _hideRetryTimer?.Stop();
            _hideRetryPending = false;
            if (_registered)
            {
                var data = CreateData();
                SHAppBarMessage(AbmRemove, ref data);
                _registered = false;
                _reservation = null;
            }

            var restoreError = RestoreWithVerification(_previousTaskbarState, _handle);
            _stateChangeStarted = false;
            TaskbarRecoveryWatchdog.NotifyCleanExit();
            if (restoreError is not null) return restoreError;

            return DeleteJournal();
        }
        finally
        {
            Volatile.Write(ref _stopInProgress, 0);
        }
    }

    public void RefreshPosition()
    {
        PositionDockWindow();
    }

    public void UpdateAutoHide(bool autoHide)
    {
        _autoHideEnabled = autoHide;
        if (!_active || Volatile.Read(ref _stopInProgress) != 0) return;
        if (!SetTaskbarState(ReplacementState, _handle))
        {
            OperationReported?.Invoke("ErrorTaskbarAutoHideToggle", true);
            return;
        }
        PositionDockWindow();
    }

    private void PositionDockWindow(bool throwOnFailure = false)
    {
        if ((!_active && !_registered) || _positioning || Volatile.Read(ref _stopInProgress) != 0)
        {
            return;
        }

        // Shell positioning can synchronously notify all appbars, including this one.
        _positioning = true;
        try
        {
            PositionDock();
        }
        catch (InvalidOperationException ex)
        {
            if (throwOnFailure) throw;
            AppLog.Warning($"Dock positioning skipped: {ex.Message}");
            OperationReported?.Invoke(ex.Message, true);
        }
        finally
        {
            _positioning = false;
        }
    }

    private void PositionDock()
    {
        var monitor = _targetMonitor != 0 ? _targetMonitor : MonitorFromWindow(_handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(monitor, ref monitorInfo))
        {
            throw new InvalidOperationException("ErrorDisplayDimensions");
        }

        var dpi = GetDpiForWindow(_handle);
        if (dpi == 0) throw new InvalidOperationException("ErrorDisplayDimensions");
        if (IsVertical)
        {
            PositionSideDock(monitorInfo.rcMonitor, dpi);
            return;
        }

        var headroomDips = _cachedHeadroomDips ??= _window.TryFindResource("DockHeadroom") as double? ?? DockHeadroomDips;
        var metrics = DockLayoutMetrics.Calculate(ShelfHeight, _nativeBottomTaskbarHeight, headroomDips, dpi);
        ReserveShelf(monitorInfo.rcMonitor, metrics, dpi);
        var height = metrics.ShelfPixels;
        var totalHeight = metrics.WindowPixels;
        var layout = new Size((monitorInfo.rcMonitor.Right - monitorInfo.rcMonitor.Left) * 96d / dpi,
            height * 96d / dpi);
        var totalHeightDips = totalHeight * 96d / dpi;
        if (_lastLayout != layout)
        {
            _lastLayout = layout;
            _window.Width = layout.Width;
            _window.Height = totalHeightDips;
            LayoutChanged?.Invoke(layout);
        }
        var monitorLeft = monitorInfo.rcMonitor.Left;
        var monitorRight = monitorInfo.rcMonitor.Right;
        var width = monitorRight - monitorLeft;
        var left = monitorLeft;
        UpdateForeground(GetForegroundWindow());
        var top = monitorInfo.rcMonitor.Bottom - totalHeight;
        var desiredPos = (left, top, width, totalHeight, _fullScreenSuppressed);
        if (_lastWindowPos == desiredPos) return;
        _lastWindowPos = desiredPos;
        if (!SetWindowPos(_handle, _fullScreenSuppressed ? new nint(1) : HwndTopmost, left, top,
                width, totalHeight,
                SwpNoActivate | SwpNoOwnerZOrder))
        {
            _lastWindowPos = null;
            throw new InvalidOperationException("ErrorDockPosition");
        }
    }

    private void ReadNativeTaskbarHeight()
    {
        var taskbar = NativeTaskbarWindow.Find();
        var data = CreateData();
        if (taskbar == 0 || SHAppBarMessage(AbmGetTaskbarPos, ref data) == 0)
            throw new InvalidOperationException("ErrorTaskbarDimensions");
        var dpi = GetDpiForWindow(taskbar);
        if (dpi == 0 || data.rc.Bottom <= data.rc.Top || data.rc.Right <= data.rc.Left)
            throw new InvalidOperationException("ErrorTaskbarDimensions");
        _nativeTaskbarAtBottom = data.uEdge == AbeBottom;
        _nativeBottomTaskbarHeight = _nativeTaskbarAtBottom ? (data.rc.Bottom - data.rc.Top) * 96d / dpi : 0;
    }

    private void ReserveShelf(Rect monitor, DockLayoutMetrics metrics, uint dpi)
    {
        if (!_registered) return;
        var nativePixels = _autoHideEnabled ? 0 : (int)Math.Ceiling(_nativeBottomTaskbarHeight * dpi / 96d);
        var extraHeight = metrics.AdditionalReservation(nativePixels, _autoHideEnabled);
        var request = (monitor.Left, monitor.Bottom, nativePixels, extraHeight);
        if (_reservation == request) return;
        _reservation = request;
        var data = CreateData();
        data.uEdge = AbeBottom;
        var bottom = monitor.Bottom - nativePixels;
        data.rc = new Rect(monitor.Left, bottom - extraHeight, monitor.Right, bottom);
        SHAppBarMessage(AbmQueryPos, ref data);
        data.rc.Top = data.rc.Bottom - extraHeight;
        SHAppBarMessage(AbmSetPos, ref data);
    }

    /// <summary>
    /// Left/right dock: a full-height column. The shelf width is reserved as an AppBar unless auto-hide is on;
    /// the headroom beside it (for magnified icons) overlaps windows like the bottom dock's headroom does.
    /// </summary>
    private void PositionSideDock(Rect monitor, uint dpi)
    {
        var thickness = Math.Max(1, (int)Math.Round(ShelfHeight * dpi / 96d));
        var headroom = (int)Math.Round(DockHeadroomDips * dpi / 96d);
        var total = thickness + headroom;
        var height = monitor.Bottom - monitor.Top;
        var edge = _edge == Models.DockEdge.Left ? AbeLeft : AbeRight;
        var request = (edge == AbeLeft ? monitor.Left : monitor.Right, monitor.Top, (int)edge, _autoHideEnabled ? 0 : thickness);
        if (_registered && _sideReservation != request)
        {
            _sideReservation = request;
            var data = CreateData();
            data.uEdge = edge;
            var reserved = _autoHideEnabled ? 0 : thickness;
            data.rc = edge == AbeLeft
                ? new Rect(monitor.Left, monitor.Top, monitor.Left + reserved, monitor.Bottom)
                : new Rect(monitor.Right - reserved, monitor.Top, monitor.Right, monitor.Bottom);
            SHAppBarMessage(AbmQueryPos, ref data);
            if (edge == AbeLeft) data.rc.Right = data.rc.Left + reserved;
            else data.rc.Left = data.rc.Right - reserved;
            SHAppBarMessage(AbmSetPos, ref data);
        }

        var layout = new Size(thickness * 96d / dpi, height * 96d / dpi);
        if (_lastLayout != layout)
        {
            _lastLayout = layout;
            _window.Width = total * 96d / dpi;
            _window.Height = layout.Height;
            LayoutChanged?.Invoke(layout);
        }

        UpdateForeground(GetForegroundWindow());
        var left = edge == AbeLeft ? monitor.Left : monitor.Right - total;
        var desiredPos = (left, monitor.Top, total, height, _fullScreenSuppressed);
        if (_lastWindowPos == desiredPos) return;
        _lastWindowPos = desiredPos;
        if (!SetWindowPos(_handle, _fullScreenSuppressed ? new nint(1) : HwndTopmost, left, monitor.Top, total, height,
                SwpNoActivate | SwpNoOwnerZOrder))
        {
            _lastWindowPos = null;
            throw new InvalidOperationException("ErrorDockPosition");
        }
    }

    /// <summary>Shelf height in DIPs for the selected dock size; the window adds headroom above it.</summary>
    public double ShelfHeight
    {
        get => _shelfHeight;
        set
        {
            if (_shelfHeight == value) return;
            _shelfHeight = value;
            if (_active && Volatile.Read(ref _stopInProgress) == 0) PositionDockWindow();
        }
    }

    private bool IsFullScreen(nint windowHandle)
    {
        if (windowHandle == 0 || !IsWindowVisible(windowHandle) || IsIconic(windowHandle))
            return false;

        var root = GetAncestor(windowHandle, GaRootOwner);
        var targetHandle = root != 0 ? root : windowHandle;

        if (MonitorFromWindow(targetHandle, MonitorDefaultToNearest) != MonitorFromWindow(_handle, MonitorDefaultToNearest))
            return false;

        var className = new StringBuilder(128);
        GetClassName(targetHandle, className, className.Capacity);
        if (className.ToString() is "Progman" or "WorkerW" or "Shell_TrayWnd" or "LogonUI" or "LockAppWindow")
            return false;
        // The lock screen covers the whole monitor but is not a full-screen app. Treating it as one hides the dock
        // with SW_HIDE and desynchronizes it from the auto-hide state, so it pops back on its own after unlocking.
        if (IsLockScreenWindow(targetHandle)) return false;

        var monitor = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(MonitorFromWindow(targetHandle, MonitorDefaultToNearest), ref monitor)
            || !GetWindowRect(targetHandle, out var rect))
            return false;

        var style = GetWindowLongPtr(targetHandle, GwlStyle).ToInt64();
        var zoomed = IsZoomed(targetHandle);
        return IsFullScreenBounds(rect, style, zoomed, monitor.rcMonitor, monitor.rcWork);
    }

    private static bool IsLockScreenWindow(nint handle)
    {
        GetWindowThreadProcessId(handle, out var processId);
        if (processId == 0) return false;
        try
        {
            using var process = Process.GetProcessById((int)processId);
            return process.ProcessName is "LogonUI" or "LockApp";
        }
        catch (ArgumentException) { return false; }
        catch (InvalidOperationException) { return false; }
    }

    internal static bool IsFullScreenBounds(Rect windowRect, long windowStyle, bool isZoomed, Rect monitorBounds, Rect workBounds)
    {
        var coversMonitor = windowRect.Left <= monitorBounds.Left + 2 && windowRect.Top <= monitorBounds.Top + 2
            && windowRect.Right >= monitorBounds.Right - 2 && windowRect.Bottom >= monitorBounds.Bottom - 2;
        if (!coversMonitor) return false;

        var workAreaNearMonitor = Math.Abs(workBounds.Left - monitorBounds.Left) <= 2
            && Math.Abs(workBounds.Top - monitorBounds.Top) <= 2
            && Math.Abs(workBounds.Right - monitorBounds.Right) <= 2
            && Math.Abs(workBounds.Bottom - monitorBounds.Bottom) <= 2;

        // Maximized desktop windows (including custom-chrome Chromium/Electron apps without WS_CAPTION)
        // carry resize-border overhang beyond monitorBounds or retain WS_CAPTION; true fullscreen apps do not.
        var hasMaximizedBorderOverhang = windowRect.Left < monitorBounds.Left - 2
            || windowRect.Top < monitorBounds.Top - 2
            || windowRect.Right > monitorBounds.Right + 2
            || windowRect.Bottom > monitorBounds.Bottom + 2;

        if (workAreaNearMonitor && isZoomed
            && ((windowStyle & WsCaption) == WsCaption || hasMaximizedBorderOverhang))
        {
            return false;
        }

        return true;
    }

    private void SetFullScreenSuppression(bool suppressed)
    {
        if (_fullScreenSuppressed == suppressed) return;
        _fullScreenSuppressed = suppressed;
        ShowWindow(_handle, suppressed ? SwHide : SwShowNoActivate);
        FullScreenChanged?.Invoke(suppressed);
    }

    private static uint GetTaskbarState()
    {
        var data = new AppBarData { cbSize = (uint)Marshal.SizeOf<AppBarData>() };
        return unchecked((uint)SHAppBarMessage(AbmGetState, ref data));
    }

    private static bool HideNativeTaskbar()
    {
        var taskbarHandle = NativeTaskbarWindow.Find();
        if (taskbarHandle == 0)
        {
            return false;
        }

        if (IsWindowVisible(taskbarHandle))
        {
            ShowWindow(taskbarHandle, SwHide);
        }

        // Secondary-monitor taskbars are best effort; the primary one decides success.
        foreach (var secondary in FindSecondaryTaskbars())
        {
            if (IsWindowVisible(secondary)) ShowWindow(secondary, SwHide);
        }

        return !IsWindowVisible(taskbarHandle);
    }

    private static List<nint> FindSecondaryTaskbars()
    {
        var handles = new List<nint>();
        var className = new StringBuilder(64);
        EnumWindows((handle, _) =>
        {
            className.Clear();
            if (GetClassName(handle, className, className.Capacity) > 0
                && className.ToString() == "Shell_SecondaryTrayWnd")
            {
                handles.Add(handle);
            }

            return true;
        }, 0);
        return handles;
    }

    private static bool ShowNativeTaskbar()
    {
        var taskbarHandle = NativeTaskbarWindow.Find();
        if (taskbarHandle == 0)
        {
            return false;
        }

        ShowWindow(taskbarHandle, SwShow);
        SetWindowPos(taskbarHandle, 0, 0, 0, 0, 0,
            SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpShowWindow | SwpFrameChanged);

        foreach (var secondary in FindSecondaryTaskbars())
        {
            ShowWindow(secondary, SwShow);
            SetWindowPos(secondary, 0, 0, 0, 0, 0,
                SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpShowWindow | SwpFrameChanged);
        }

        return true;
    }

    private static bool IsNativeTaskbarVisible()
    {
        var taskbar = NativeTaskbarWindow.Find();
        return taskbar != 0 && IsWindowVisible(taskbar);
    }

    /// <summary>
    /// Explorer can re-apply auto-hide right after we show the taskbar (ABN_STATECHANGE / ABN_POSCHANGE), and
    /// secondary tray windows can appear a moment later. Re-assert state and visibility a few times and require
    /// two consecutive clean observations before declaring success.
    /// </summary>
    private static string? RestoreWithVerification(uint state, nint fallbackHandle)
    {
        for (var attempt = 0; attempt < RestoreAttempts; attempt++)
        {
            var restored = TryRestoreOnce(state, fallbackHandle);
            if (restored)
            {
                Thread.Sleep(RestoreAttemptDelayMs);
                if (TryRestoreOnce(state, fallbackHandle)) return null;
            }

            if (attempt < RestoreAttempts - 1) Thread.Sleep(RestoreAttemptDelayMs);
        }

        return "ErrorTaskbarRestoreFailed";
    }

    private static bool TryRestoreOnce(uint state, nint fallbackHandle)
    {
        var stateRestored = SetTaskbarState(state, fallbackHandle);
        var taskbarShown = ShowNativeTaskbar();
        return stateRestored && taskbarShown && IsNativeTaskbarVisible();
    }

    private void EnforceNativeTaskbarHidden()
    {
        if (!_active || Volatile.Read(ref _stopInProgress) != 0) return;
        if (!SetTaskbarState(ReplacementState, _handle))
        {
            OperationReported?.Invoke("TaskbarAutoHideReenabled", false);
            return;
        }

        if (!HideNativeTaskbar()) RetryHideNativeTaskbar();
    }

    // Explorer briefly re-shows an auto-hidden taskbar when the pointer touches the screen edge; retry before reporting.
    private void RetryHideNativeTaskbar()
    {
        if (_hideRetryPending || _disposed) return;
        _hideRetryPending = true;
        _hideRetryAttempts = 0;
        if (_hideRetryTimer is null)
        {
            _hideRetryTimer = new DispatcherTimer(DispatcherPriority.Background, _window.Dispatcher)
            {
                Interval = TimeSpan.FromMilliseconds(300)
            };
            _hideRetryTimer.Tick += HideRetryTimer_Tick;
        }

        _hideRetryTimer.Start();
    }

    private void HideRetryTimer_Tick(object? sender, EventArgs e)
    {
        _hideRetryAttempts++;
        if (!_active || Volatile.Read(ref _stopInProgress) != 0)
        {
            _hideRetryTimer?.Stop();
            _hideRetryPending = false;
            return;
        }

        var hidden = HideNativeTaskbar();
        if (hidden || _hideRetryAttempts >= 3)
        {
            _hideRetryTimer?.Stop();
            _hideRetryPending = false;
            if (!hidden) OperationReported?.Invoke("ErrorTaskbarRehide", true);
        }
    }

    private static bool SetTaskbarState(uint state, nint windowHandle)
    {
        // Explorer can notify us about our own update; an identical state must not publish another one.
        var current = GetTaskbarState();
        if ((current & SupportedTaskbarState) == (state & SupportedTaskbarState) ||
            (current & AbsAutoHide) == (state & AbsAutoHide))
        {
            return true;
        }

        var taskbarHandle = NativeTaskbarWindow.Find();
        var data = new AppBarData
        {
            cbSize = (uint)Marshal.SizeOf<AppBarData>(),
            hWnd = taskbarHandle != 0 ? taskbarHandle : windowHandle,
            lParam = unchecked((nint)state)
        };
        SHAppBarMessage(AbmSetState, ref data);
        current = GetTaskbarState();
        return (current & SupportedTaskbarState) == (state & SupportedTaskbarState) ||
               (current & AbsAutoHide) == (state & AbsAutoHide);
    }

    private AppBarData CreateData() => new()
    {
        cbSize = (uint)Marshal.SizeOf<AppBarData>(),
        hWnd = _handle
    };

    private static string JournalPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "OpenTaskbar",
        "taskbar-state.txt");

    private static void WriteJournal(uint state)
    {
        var path = JournalPath;
        var directory = Path.GetDirectoryName(path);
        if (directory is null)
        {
            throw new IOException("ErrorTaskbarJournalPath");
        }

        Directory.CreateDirectory(directory);
        var temporaryPath = path + ".tmp";
        File.WriteAllText(temporaryPath, state.ToString(CultureInfo.InvariantCulture));
        File.Move(temporaryPath, path, overwrite: true);
    }

    private static uint ReadJournal() => ParseJournal(File.ReadAllText(JournalPath));

    internal static uint ParseJournal(string text)
    {
        if (!uint.TryParse(text.Trim(), NumberStyles.None, CultureInfo.InvariantCulture, out var state)
            || (state & ~SupportedTaskbarState) != 0)
        {
            throw new InvalidDataException("ErrorTaskbarJournalInvalid");
        }

        return state;
    }

    /// <summary>A missing or damaged journal must restore a visible taskbar, never a guessed hidden state.</summary>
    internal static uint FallbackState(string? journalText)
    {
        if (journalText is null) return 0;
        try
        {
            return ParseJournal(journalText);
        }
        catch (InvalidDataException)
        {
            return 0;
        }
    }

    private static string? DeleteJournal()
    {
        try
        {
            if (File.Exists(JournalPath))
            {
                File.Delete(JournalPath);
            }

            return null;
        }
        catch (IOException)
        {
            return "ErrorTaskbarJournalDelete";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorTaskbarJournalAccess";
        }
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        var error = Stop();
        if (error is not null) Trace.TraceError(error);
        if (IsActive) return;
        _disposed = true;
        _hideRetryTimer?.Stop();
        if (_hideRetryTimer is not null)
        {
            _hideRetryTimer.Tick -= HideRetryTimer_Tick;
            _hideRetryTimer = null;
        }
        Interlocked.CompareExchange(ref _current, null, this);
        AppDomain.CurrentDomain.ProcessExit -= Process_Exit;
    }

    private void Process_Exit(object? sender, EventArgs e)
    {
        var error = Stop();
        if (error is not null) Trace.TraceError(error);
        // ProcessExit can race with a Stop() on another thread. A leftover journal means the taskbar may still be
        // hidden, so use the journal-based path which does not depend on instance state or _stopInProgress.
        if (File.Exists(JournalPath))
        {
            error = ForceRestoreNativeTaskbar();
            if (error is not null) Trace.TraceError(error);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct AppBarData
    {
        public uint cbSize;
        public nint hWnd;
        public uint uCallbackMessage;
        public uint uEdge;
        public Rect rc;
        public nint lParam;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct Rect(int left, int top, int right, int bottom)
    {
        public int Left = left;
        public int Top = top;
        public int Right = right;
        public int Bottom = bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public uint cbSize;
        public Rect rcMonitor;
        public Rect rcWork;
        public uint dwFlags;
    }

    [DllImport("shell32.dll")]
    private static extern nuint SHAppBarMessage(uint message, ref AppBarData data);

    [DllImport("user32.dll")]
    private static extern nint MonitorFromWindow(nint windowHandle, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(nint monitor, ref MonitorInfo monitorInfo);

    private delegate bool EnumWindowsProc(nint windowHandle, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsProc callback, nint lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "FindWindowW")]
    private static extern nint FindWindow(string? className, string? windowName);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint flags);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsZoomed(nint windowHandle);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr(nint windowHandle, int index);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint windowHandle, out Rect rect);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(nint windowHandle, StringBuilder name, int length);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint windowHandle, nint insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint windowHandle, int command);
}
