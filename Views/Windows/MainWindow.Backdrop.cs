using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>PROTOTYPE: keeps the acrylic <see cref="DockAcrylicBackdrop"/> under the dock frame as it moves, resizes and slides.</summary>
public partial class MainWindow
{
    private const int WmWindowPosChanged = 0x0047;
    private DockAcrylicBackdrop? _backdrop;

    private void InitializeDockBackdrop(HwndSource source)
    {
        _backdrop = DockAcrylicBackdrop.TryCreate(source.Handle, _preferences.Current.Theme);
        if (_backdrop is null) return;

        // The blur carries the glass now; the dock's own tint gets lighter so the two do not stack into a dark slab.
        ThemeManager.SetDockGlass(blurred: true);
        // The glass has the system's rounded corners; the dock frame takes the same radius so nothing sticks out.
        ApplySizeProfile();
        // Auto-hide slides the surface with this transform; Changed fires on every animation frame.
        DockSurfaceTranslate.Changed += (_, _) => SyncBackdrop();
        DockFrame.LayoutUpdated += (_, _) => SyncBackdrop();
        IsVisibleChanged += (_, _) => SyncBackdrop(restack: true);
        source.AddHook(BackdropWindowHook);
        Closed += (_, _) =>
        {
            _backdrop?.Dispose();
            _backdrop = null;
        };
        SyncBackdrop(restack: true);
    }

    private nint BackdropWindowHook(nint window, int message, nint wParam, nint lParam, ref bool handled)
    {
        // The dock moved, resized or changed its z-order: keep the backdrop right below it.
        if (message == WmWindowPosChanged) SyncBackdrop(restack: true);
        return 0;
    }

    private void SyncBackdrop(bool restack = false)
    {
        if (_backdrop is null || _source is not { Handle: not 0 } source) return;
        _backdrop.SetTheme(_preferences.Current.Theme);
        if (!BackdropGetWindowRect(source.Handle, out var clip))
            return;
        _backdrop.Sync(GetDockDrawnBounds(), (clip.Left, clip.Top, clip.Right, clip.Bottom),
            IsVisible && WindowState != WindowState.Minimized, restack);
    }

    /// <summary>The dock frame in screen pixels as drawn, slide offset included (unlike <see cref="GetDockPhysicalBounds"/>).</summary>
    private (int Left, int Top, int Right, int Bottom)? GetDockDrawnBounds()
    {
        if (!IsLoaded || DockFrame.ActualWidth <= 0 || DockFrame.ActualHeight <= 0) return null;
        try
        {
            var topLeft = DockFrame.PointToScreen(new Point(0, 0));
            var bottomRight = DockFrame.PointToScreen(new Point(DockFrame.ActualWidth, DockFrame.ActualHeight));
            return ((int)Math.Round(topLeft.X), (int)Math.Round(topLeft.Y), (int)Math.Round(bottomRight.X), (int)Math.Round(bottomRight.Y));
        }
        catch (InvalidOperationException)
        {
            return null;
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct BackdropRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll", EntryPoint = "GetWindowRect")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool BackdropGetWindowRect(nint window, out BackdropRect rect);
}
