using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Interop;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Opens Windows' own flyouts and settings pages, which stand in for the hidden taskbar's tray area.
/// </summary>
public static class ShellShortcuts
{
    public const ushort VkLeftWindows = 0x5B;
    public const ushort VkControl = 0x11;
    public const ushort VkLeft = 0x25;
    public const ushort VkRight = 0x27;
    public const ushort VkTab = 0x09;
    public const ushort VkD = 0x44;
    private const ushort VkA = 0x41;
    private const ushort VkN = 0x4E;
    private const ushort VkP = 0x50;
    private const ushort VkS = 0x53;
    private const uint MonitorDefaultToNearest = 2;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const int ErrorCancelled = 1223;
    private static CancellationTokenSource? _repositionCts;
    private static CancellationTokenSource? _searchRepositionCts;
    public static Func<bool>? IsMediaWidgetActive { get; set; }

    /// <summary>Win+A: Quick Settings (Windows 11) or Action Center (Windows 10), anchored above the clicked button when given.</summary>
    public static string? OpenQuickSettings(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = SendWindowsShortcut(VkA);
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Opens Windows' native available Wi-Fi networks flyout without throwing the user into full Settings.</summary>
    public static string? OpenAvailableNetworks(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = Start("ms-availablenetworks:", "ErrorSettingsPageOpen");
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Win+N: notification center and calendar, opened natively via Windows shortcut.</summary>
    public static string? OpenNotificationCenter(FrameworkElement? anchor = null)
    {
        AppLog.Info("OpenNotificationCenter: invoked.");
        EnsureShellExperienceHost();
        var error = SendWindowsShortcut(VkN);
        if (error is not null) AppLog.Warning($"OpenNotificationCenter failed: {error}");
        return error;
    }

    private static void EnsureShellExperienceHost()
    {
        try
        {
            if (Process.GetProcessesByName("ShellExperienceHost").Length == 0)
            {
                AppLog.Info("EnsureShellExperienceHost: launching ShellExperienceHost package...");
                using var _ = Process.Start(new ProcessStartInfo("explorer.exe", "shell:AppsFolder\\Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy!App") { UseShellExecute = true });
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"EnsureShellExperienceHost exception: {ex.Message}");
        }
    }

    /// <summary>Win+P: display projection flyout, anchored above the clicked button when given.</summary>
    public static string? OpenProjection(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = SendWindowsShortcut(VkP);
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Win+S: Windows Search flyout, opened natively via Windows shortcut and anchored above the clicked button when given.</summary>
    public static string? OpenWindowsSearch(FrameworkElement? anchor = null)
    {
        AppLog.Info("OpenWindowsSearch: invoked.");
        EnsureNativeTaskbarHidden();
        var error = SendWindowsShortcut(VkS);
        EnsureNativeTaskbarHidden();
        if (error is null && anchor is not null)
        {
            ScheduleSearchReposition(anchor);
        }
        if (error is not null) AppLog.Warning($"OpenWindowsSearch failed: {error}");
        return error;
    }

    /// <summary>Checks whether Windows Search flyout is currently open and uncloaked.</summary>
    public static bool IsSearchFlyoutOpen() => FindSearchWindows().Count > 0;

    /// <summary>Closes Windows Search flyout if currently open.</summary>
    public static void CloseWindowsSearch()
    {
        if (IsSearchFlyoutOpen())
        {
            SendWindowsShortcut(VkS);
            EnsureNativeTaskbarHidden();
        }
    }

    /// <summary>Forwards a virtual key down/up to the open Windows Search flyout.</summary>
    public static void ForwardKeyToSearch(ushort vk)
    {
        var windows = FindSearchWindows();
        if (windows.Count == 0) return;
        foreach (var hwnd in windows)
        {
            PostMessage(hwnd, WmKeyDown, (nint)vk, 0);
            PostMessage(hwnd, WmKeyUp, (nint)vk, 0);
        }
    }

    /// <summary>Synchronizes the dock search text query into Windows Search edit control.</summary>
    public static void SyncSearchQuery(string query)
    {
        Task.Run(() =>
        {
            try
            {
                var windows = FindSearchWindows();
                if (windows.Count == 0) return;

                foreach (var hwnd in windows)
                {
                    try
                    {
                        var root = AutomationElement.FromHandle(hwnd);
                        if (root is null) continue;

                        var editCondition = new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Edit);
                        var edit = root.FindFirst(TreeScope.Descendants, editCondition);
                        if (edit is not null)
                        {
                            if (edit.TryGetCurrentPattern(ValuePattern.Pattern, out var pattern) && pattern is ValuePattern vp)
                            {
                                vp.SetValue(query);
                                return;
                            }
                        }
                    }
                    catch (Exception ex)
                    {
                        AppLog.Warning($"SyncSearchQuery element error: {ex.Message}");
                    }
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"SyncSearchQuery failed: {ex.Message}");
            }
        });
    }

    private static void EnsureNativeTaskbarHidden()
    {
        var taskbar = NativeTaskbarWindow.Find();
        if (taskbar != 0 && IsWindowVisible(taskbar))
        {
            ShowWindow(taskbar, SwHide);
            AppLog.Info("EnsureNativeTaskbarHidden: Shell_TrayWnd hidden.");
        }
        var secondary = FindWindow("Shell_SecondaryTrayWnd", null);
        if (secondary != 0 && IsWindowVisible(secondary))
        {
            ShowWindow(secondary, SwHide);
        }
    }

    private static List<nint> FindSearchWindows()
    {
        var list = new List<nint>();
        try
        {
            var pids = Process.GetProcessesByName("SearchHost")
                .Concat(Process.GetProcessesByName("SearchApp"))
                .Select(p => (uint)p.Id)
                .ToHashSet();

            if (pids.Count > 0)
            {
                EnumWindows((hwnd, _) =>
                {
                    if (!IsWindowVisible(hwnd)) return true;
                    if (IsWindowCloaked(hwnd)) return true;
                    GetWindowThreadProcessId(hwnd, out var pid);
                    if (pids.Contains(pid))
                    {
                        if (GetWindowRect(hwnd, out var rect))
                        {
                            var w = rect.Right - rect.Left;
                            var h = rect.Bottom - rect.Top;
                            if (w > 40 && h > 20)
                            {
                                list.Add(hwnd);
                            }
                        }
                    }
                    return true;
                }, 0);
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"FindSearchWindows exception: {ex.Message}");
        }

        if (list.Count == 0)
        {
            foreach (var title in new[] { "Search", "Arama", "Ara" })
            {
                var h = FindWindow("Windows.UI.Core.CoreWindow", title);
                if (h != 0 && IsWindowVisible(h) && !IsWindowCloaked(h))
                {
                    list.Add(h);
                    break;
                }
            }
        }

        return list;
    }

    private static void ScheduleSearchReposition(FrameworkElement anchor)
    {
        AppLog.Info("ScheduleSearchReposition: scheduled.");
        var next = new CancellationTokenSource();
        var previous = Interlocked.Exchange(ref _searchRepositionCts, next);
        previous?.Cancel();
        previous?.Dispose();
        _ = RepositionSearchAsync(anchor, next.Token);
    }

    private static async Task RepositionSearchAsync(FrameworkElement anchor, CancellationToken cancellation)
    {
        if (PresentationSource.FromVisual(anchor) is not HwndSource source || source.Handle == 0)
        {
            AppLog.Warning("RepositionSearchAsync: PresentationSource is null.");
            return;
        }

        Point topLeft;
        Point bottomRight;
        try
        {
            topLeft = anchor.PointToScreen(new Point(0, 0));
            bottomRight = anchor.PointToScreen(new Point(anchor.ActualWidth, anchor.ActualHeight));
        }
        catch (InvalidOperationException ex)
        {
            AppLog.Warning($"RepositionSearchAsync PointToScreen failed: {ex.Message}");
            return;
        }

        var monitor = MonitorFromWindow(source.Handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref monitorInfo))
        {
            AppLog.Warning("RepositionSearchAsync: GetMonitorInfo failed.");
            return;
        }

        var edge = DockFlyout.Edge;
        AppLog.Info($"RepositionSearchAsync: anchor bounds [{topLeft.X:F0},{topLeft.Y:F0} - {bottomRight.X:F0},{bottomRight.Y:F0}], edge: {edge}");

        try
        {
            for (var attempt = 0; attempt < 35; attempt++)
            {
                await Task.Delay(20, cancellation);
                if (cancellation.IsCancellationRequested) return;

                EnsureNativeTaskbarHidden();

                var searchWindows = FindSearchWindows();
                if (searchWindows.Count == 0) continue;

                AppLog.Info($"RepositionSearchAsync: attempt {attempt} found {searchWindows.Count} window(s).");

                var mainHwnd = searchWindows.OrderByDescending(h =>
                {
                    if (GetWindowRect(h, out var r)) return (long)(r.Right - r.Left) * (r.Bottom - r.Top);
                    return 0L;
                }).First();

                if (!GetWindowRect(mainHwnd, out var rect)) continue;
                var width = rect.Right - rect.Left;
                var height = rect.Bottom - rect.Top;
                if (width < 100 || height < 100) continue;

                var centerX = (int)Math.Round((topLeft.X + bottomRight.X) / 2);
                var centerY = (int)Math.Round((topLeft.Y + bottomRight.Y) / 2);
                int targetX;
                int targetY;
                var minY = monitorInfo.Work.Top + 8;
                var maxY = Math.Max(minY, monitorInfo.Work.Bottom - height - 8);

                switch (edge)
                {
                    case DockEdge.Left:
                        targetX = (int)Math.Round(bottomRight.X) + 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    case DockEdge.Right:
                        targetX = (int)Math.Round(topLeft.X) - width - 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    default:
                        var minX = monitorInfo.Work.Left + 8;
                        var maxX = Math.Max(minX, monitorInfo.Work.Right - width - 8);
                        targetX = Math.Clamp(centerX - width / 2, minX, maxX);
                        targetY = Math.Clamp((int)Math.Round(topLeft.Y) - height - 12, minY, maxY);
                        break;
                }

                AppLog.Info($"RepositionSearchAsync: moving main window {mainHwnd:X} ({width}x{height}) from ({rect.Left},{rect.Top}) to ({targetX},{targetY})");
                SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);

                foreach (var aux in searchWindows.Where(h => h != mainHwnd))
                {
                    if (GetWindowRect(aux, out var auxRect))
                    {
                        var auxW = auxRect.Right - auxRect.Left;
                        var auxH = auxRect.Bottom - auxRect.Top;
                        var auxX = Math.Clamp(centerX - auxW / 2, monitorInfo.Work.Left + 8, monitorInfo.Work.Right - auxW - 8);
                        var auxY = Math.Clamp((int)Math.Round(topLeft.Y) - auxH - 8, minY, maxY);
                        AppLog.Info($"RepositionSearchAsync: moving aux window {aux:X} ({auxW}x{auxH}) to ({auxX},{auxY})");
                        SetWindowPos(aux, 0, auxX, auxY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                    }
                }

                for (var frame = 0; frame < 5; frame++)
                {
                    await Task.Delay(30, cancellation);
                    EnsureNativeTaskbarHidden();
                    SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                }
                break;
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    private static bool IsControlCenterVisible()
    {
        var hwnd = FindWindow("ControlCenterWindow", null);
        return hwnd != 0 && IsWindowVisible(hwnd);
    }

    private static void ScheduleControlCenterReposition(FrameworkElement anchor)
    {
        var next = new CancellationTokenSource();
        var previous = Interlocked.Exchange(ref _repositionCts, next);
        previous?.Cancel();
        previous?.Dispose();
        _ = RepositionControlCenterAsync(anchor, next.Token);
    }

    private static async Task RepositionControlCenterAsync(FrameworkElement anchor, CancellationToken cancellation)
    {
        if (PresentationSource.FromVisual(anchor) is not HwndSource source || source.Handle == 0) return;
        Point topLeft;
        Point bottomRight;
        try
        {
            topLeft = anchor.PointToScreen(new Point(0, 0));
            bottomRight = anchor.PointToScreen(new Point(anchor.ActualWidth, anchor.ActualHeight));
        }
        catch (InvalidOperationException)
        {
            return;
        }

        var monitor = MonitorFromWindow(source.Handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref monitorInfo)) return;
        var edge = DockFlyout.Edge;
        var movedOnce = false;

        try
        {
            for (var attempt = 0; attempt < 24; attempt++)
            {
                await Task.Delay(25, cancellation);
                if (cancellation.IsCancellationRequested) return;
                var hwnd = FindWindow("ControlCenterWindow", null);
                if (hwnd == 0 || !IsWindowVisible(hwnd) || !GetWindowRect(hwnd, out var rect)) continue;
                var width = rect.Right - rect.Left;
                var height = rect.Bottom - rect.Top;
                if (width < 80 || height < 80) continue;

                var centerX = (int)Math.Round((topLeft.X + bottomRight.X) / 2);
                var centerY = (int)Math.Round((topLeft.Y + bottomRight.Y) / 2);
                int targetX;
                int targetY;
                var minY = monitorInfo.Work.Top + 8;
                var maxY = Math.Max(minY, monitorInfo.Work.Bottom - height - 8);
                switch (edge)
                {
                    case DockEdge.Left:
                        targetX = (int)Math.Round(bottomRight.X) + 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    case DockEdge.Right:
                        targetX = (int)Math.Round(topLeft.X) - width - 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    default:
                        var minX = monitorInfo.Work.Left + 8;
                        var maxX = Math.Max(minX, monitorInfo.Work.Right - width - 8);
                        targetX = Math.Clamp(centerX - width / 2, minX, maxX);
                        targetY = Math.Clamp((int)Math.Round(topLeft.Y) - height - 12, minY, maxY);
                        break;
                }

                SetWindowPos(hwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                if (movedOnce) break;
                movedOnce = true;
                // Re-apply once more after ShellHost finishes its entrance animation.
                await Task.Delay(75, cancellation);
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    public static string? OpenUri(string uri) => Start(uri, "ErrorSettingsPageOpen");

    /// <summary>
    /// Task Manager by its full path, so a program named taskmgr elsewhere on the search path is never started.
    /// The Shell handles its elevation request.
    /// </summary>
    public static string? OpenTaskManager() => Start(
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "Taskmgr.exe"), "ErrorTaskManagerOpen");

    private static string? Start(string target, string failureKey)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(target) { UseShellExecute = true });
            return null;
        }
        catch (Win32Exception ex) when (ex.NativeErrorCode == ErrorCancelled)
        {
            // The user declined the elevation prompt; that is a choice, not a failure.
            return null;
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Cannot open {target}: {ex.Message}");
            return UiText.Get(failureKey);
        }
    }

    private static string? SendWindowsShortcut(ushort key) => SendChord(VkLeftWindows, key);

    /// <summary>Presses the keys in order and releases them in reverse, with a guaranteed modifier cleanup.</summary>
    public static string? SendChord(params ushort[] keys) => SyntheticKeyboard.SendChord(keys);

    /// <summary>Drops modifiers left down by a shell shortcut interrupted by a lock or focus transition.</summary>
    public static void ReleaseStuckModifiers() => SyntheticKeyboard.ReleaseAll();

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeRect
    {
        public int Left, Top, Right, Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public int Size;
        public NativeRect Monitor;
        public NativeRect Work;
        public uint Flags;
    }

    private const int SwHide = 0;
    private const int DwmwaCloaked = 14;
    private const uint WmKeyDown = 0x0100;
    private const uint WmKeyUp = 0x0101;

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(nint hWnd, uint msg, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint hWnd, int nCmdShow);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);

    private static bool IsWindowCloaked(nint hwnd) =>
        DwmGetWindowAttribute(hwnd, DwmwaCloaked, out var cloaked, sizeof(int)) == 0 && cloaked != 0;

    private delegate bool EnumWindowsProc(nint hWnd, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsProc enumProc, nint lParam);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern nint FindWindow(string? lpClassName, string? lpWindowName);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint hWnd, out NativeRect lpRect);

    [DllImport("user32.dll")]
    private static extern nint MonitorFromWindow(nint hwnd, uint dwFlags);

    [DllImport("user32.dll", EntryPoint = "GetMonitorInfoW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(nint hMonitor, ref MonitorInfo lpmi);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint hWnd, nint hWndInsertAfter, int x, int y, int cx, int cy, uint uFlags);
}
