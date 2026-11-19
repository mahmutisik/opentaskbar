using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// PROTOTYPE: frosted glass behind the dock. The dock window is layered (per-pixel alpha), and Windows does not blur
/// behind layered windows, so a separate plain window with a blur-behind accent sits directly below the dock in
/// z-order, sized to the dock frame. It is never activated and never takes clicks (the dock covers it). Where the dock
/// slides out of its own window (auto-hide), the backdrop is clipped the same way.
/// The Windows 11 system acrylic is not used: it falls back to a solid colour whenever its window is inactive, and
/// this window never is. The blur ignores window regions, so the corners are the system's rounded window corners and
/// the dock takes the same radius (<see cref="CornerRadiusDip"/>).
/// </summary>
internal sealed class DockAcrylicBackdrop : IDisposable
{
    private const int WsExToolWindow = 0x00000080;
    private const int WsExNoActivate = 0x08000000;
    private const int WsExTopmost = 0x00000008;
    private const int GwlExStyle = -20;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpShowWindow = 0x0040;
    private const uint SwpNoOwnerZOrder = 0x0200;
    private static readonly nint HwndTopmost = new(-1);
    private static readonly nint HwndNoTopmost = new(-2);

    private readonly Window _window;
    private readonly nint _handle;
    private readonly nint _dockHandle;
    private AppTheme _theme;
    private (int X, int Y, int Width, int Height)? _placed;

    /// <summary>Radius of Windows 11's rounded window corners (DWMWCP_ROUND) at 96 DPI; the dock frame uses it too.</summary>
    public const double CornerRadiusDip = 8;

    private DockAcrylicBackdrop(Window window, nint handle, nint dockHandle, AppTheme theme)
    {
        _window = window;
        _handle = handle;
        _dockHandle = dockHandle;
        _theme = theme;
    }

    /// <summary>Creates the backdrop when the system acrylic backdrop is available (Windows 11 22H2 and later).</summary>
    public static DockAcrylicBackdrop? TryCreate(nint dockHandle, AppTheme theme)
    {
        if (!OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22621) || SystemParameters.HighContrast) return null;
        var window = new Window
        {
            Title = "OpenTaskbar dock backdrop",
            WindowStyle = WindowStyle.None,
            ResizeMode = ResizeMode.NoResize,
            AllowsTransparency = false,
            ShowInTaskbar = false,
            ShowActivated = false,
            Focusable = false,
            IsHitTestVisible = false,
            Background = Brushes.Transparent,
            Left = -32000,
            Top = -32000,
            Width = 1,
            Height = 1
        };
        var handle = new WindowInteropHelper(window).EnsureHandle();
        SetWindowLongPtr(handle, GwlExStyle, GetWindowLongPtr(handle, GwlExStyle) | WsExToolWindow | WsExNoActivate);

        var backdrop = new DockAcrylicBackdrop(window, handle, dockHandle, theme);
        if (!backdrop.ApplyMaterial())
        {
            window.Close();
            return null;
        }

        // Shown once, off screen; afterwards it is only moved on and off screen, so WPF keeps rendering its
        // transparent client area for the acrylic to show through.
        window.Show();
        AppLog.Info("Dock acrylic backdrop enabled.");
        return backdrop;
    }

    private bool ApplyMaterial()
    {
        var dark = _theme == AppTheme.Dark ? 1 : 0;
        var corners = 2; // DWMWCP_ROUND
        var noBorder = unchecked((int)0xFFFFFFFE); // DWMWA_COLOR_NONE: no system border line around the glass.
        var margins = new Margins(-1);
        DwmSetWindowAttribute(_handle, 20, ref dark, sizeof(int));
        var cornerResult = DwmSetWindowAttribute(_handle, 33, ref corners, sizeof(int));
        DwmSetWindowAttribute(_handle, 34, ref noBorder, sizeof(int));
        var frameResult = DwmExtendFrameIntoClientArea(_handle, ref margins);
        var blurred = EnableBlurBehind(_handle);
        if (cornerResult < 0 || frameResult < 0 || !blurred)
        {
            Trace.TraceWarning("Dock backdrop unavailable: corners=0x{0:X8}, frame=0x{1:X8}, blur={2}", cornerResult, frameResult, blurred);
            return false;
        }

        if (HwndSource.FromHwnd(_handle)?.CompositionTarget is { } target) target.BackgroundColor = Colors.Transparent;
        return true;
    }

    public void SetTheme(AppTheme theme)
    {
        if (theme == _theme) return;
        _theme = theme;
        var dark = theme == AppTheme.Dark ? 1 : 0;
        DwmSetWindowAttribute(_handle, 20, ref dark, sizeof(int));
    }

    /// <summary>
    /// Places the backdrop under the dock frame. <paramref name="frame"/> is the frame in screen pixels, as drawn
    /// (slide offsets included); <paramref name="clip"/> is the dock window, which clips what the dock can show.
    /// </summary>
    public void Sync((int Left, int Top, int Right, int Bottom)? frame, (int Left, int Top, int Right, int Bottom) clip, bool visible, bool restack)
    {
        if (!visible || frame is not { } f)
        {
            Hide();
            return;
        }

        // Inset one pixel so the glass edge sits under the dock's antialiased rim.
        var left = f.Left + 1;
        var top = f.Top + 1;
        var right = f.Right - 1;
        var bottom = f.Bottom - 1;
        var visibleLeft = Math.Max(left, clip.Left);
        var visibleTop = Math.Max(top, clip.Top);
        var visibleRight = Math.Min(right, clip.Right);
        var visibleBottom = Math.Min(bottom, clip.Bottom);
        if (visibleRight - visibleLeft < 2 || visibleBottom - visibleTop < 2)
        {
            Hide();
            return;
        }

        var width = visibleRight - visibleLeft;
        var height = visibleBottom - visibleTop;
        var placement = (visibleLeft, visibleTop, width, height);
        // Layout passes call this often; only touch the window when it moved or the dock changed its z-order.
        if (!restack && _placed == placement) return;

        // Match the dock's band (topmost or not) first; a window is only placed right below another in its own band.
        var dockTopmost = (GetWindowLongPtr(_dockHandle, GwlExStyle) & WsExTopmost) != 0;
        var ownTopmost = (GetWindowLongPtr(_handle, GwlExStyle) & WsExTopmost) != 0;
        if (dockTopmost != ownTopmost)
            SetWindowPos(_handle, dockTopmost ? HwndTopmost : HwndNoTopmost, 0, 0, 0, 0, SwpNoMove | SwpNoSize | SwpNoActivate | SwpNoOwnerZOrder);

        SetWindowPos(_handle, _dockHandle, visibleLeft, visibleTop, width, height, SwpNoActivate | SwpNoOwnerZOrder | SwpShowWindow);
        _placed = placement;
    }

    private void Hide()
    {
        if (_placed is null) return;
        _placed = null;
        SetWindowPos(_handle, 0, -32000, -32000, 1, 1, SwpNoActivate | SwpNoOwnerZOrder | SwpNoZOrder);
    }

    public void Dispose()
    {
        _placed = null;
        _window.Close();
    }

    /// <summary>ACCENT_ENABLE_BLURBEHIND through SetWindowCompositionAttribute: a plain blur that stays on while inactive.</summary>
    private static bool EnableBlurBehind(nint window)
    {
        var accent = new AccentPolicy { State = 3 };
        var size = Marshal.SizeOf<AccentPolicy>();
        var pointer = Marshal.AllocHGlobal(size);
        try
        {
            Marshal.StructureToPtr(accent, pointer, false);
            var data = new CompositionAttributeData { Attribute = 19, Data = pointer, Size = size }; // WCA_ACCENT_POLICY
            return SetWindowCompositionAttribute(window, ref data) != 0;
        }
        finally
        {
            Marshal.FreeHGlobal(pointer);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct AccentPolicy
    {
        public int State;
        public int Flags;
        public uint GradientColor;
        public int AnimationId;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct CompositionAttributeData
    {
        public int Attribute;
        public nint Data;
        public int Size;
    }

    [DllImport("user32.dll")]
    private static extern int SetWindowCompositionAttribute(nint window, ref CompositionAttributeData data);

    [StructLayout(LayoutKind.Sequential)]
    private struct Margins(int value)
    {
        public int Left = value;
        public int Right = value;
        public int Top = value;
        public int Bottom = value;
    }

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(nint window, uint attribute, ref int value, uint size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(nint window, ref Margins margins);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr(nint window, int index);

    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    private static extern nint SetWindowLongPtr(nint window, int index, nint value);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint window, nint insertAfter, int x, int y, int width, int height, uint flags);



}
