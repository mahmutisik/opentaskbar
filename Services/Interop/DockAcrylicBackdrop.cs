using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Windows;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Frosted glass behind the dock. The dock window is layered (per-pixel alpha), and Windows does not blur
/// behind layered windows, so a separate window with a blur-behind accent sits directly below the dock in z-order,
/// sized to the dock frame. It is never activated and never takes clicks (the dock covers it, and it reports itself
/// as transparent to hit testing). Where the dock slides out of its own window (auto-hide), the backdrop is clipped
/// the same way.
/// The Windows 11 system acrylic is not used: it falls back to a solid colour whenever its window is inactive, and
/// this window never is. The blur ignores window regions, so the corners are the system's rounded window corners and
/// the dock takes the same radius (<see cref="CornerRadiusDip"/>).
/// It is a bare Win32 window, not a WPF one: it follows the dock on every animation frame, and a WPF window would
/// re-render and resize its swap chain each time its size changes, stalling the dock by ~100 ms per slide.
/// </summary>
internal sealed class DockAcrylicBackdrop : IDisposable
{
    private const string ClassName = "OpenTaskbarDockBackdrop";
    private const uint WsPopup = 0x80000000;
    private const int WsExToolWindow = 0x00000080;
    private const int WsExNoActivate = 0x08000000;
    private const int WsExTopmost = 0x00000008;
    private const int GwlExStyle = -20;
    private const uint WmEraseBackground = 0x0014;
    private const uint WmPaint = 0x000F;
    private const uint WmNcHitTest = 0x0084;
    private const nint HitTransparent = -1;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoRedraw = 0x0008;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpShowWindow = 0x0040;
    private const uint SwpHideWindow = 0x0080;
    private const uint SwpNoCopyBits = 0x0100;
    private const uint SwpNoOwnerZOrder = 0x0200;
    private static readonly nint HwndTopmost = new(-1);
    private static readonly nint HwndNoTopmost = new(-2);
    // Kept in a static field so the delegate handed to Windows is never collected.
    private static readonly WindowProcedure Procedure = BackdropProcedure;
    private static bool _classRegistered;

    private readonly nint _handle;
    private readonly nint _dockHandle;
    private AppTheme _theme;
    private (int X, int Y, int Width, int Height)? _placed;

    /// <summary>Radius of Windows 11's rounded window corners (DWMWCP_ROUND) at 96 DPI; the dock frame uses it too.</summary>
    public const double CornerRadiusDip = 8;

    private DockAcrylicBackdrop(nint handle, nint dockHandle, AppTheme theme)
    {
        _handle = handle;
        _dockHandle = dockHandle;
        _theme = theme;
    }

    /// <summary>The blur needs Windows 11 22H2 or later, and stays off in high contrast.</summary>
    public static bool IsSupported => OperatingSystem.IsWindowsVersionAtLeast(10, 0, 22621) && !SystemParameters.HighContrast;

    /// <summary>Creates the backdrop where it is supported; null where the blur is unavailable.</summary>
    public static DockAcrylicBackdrop? TryCreate(nint dockHandle, AppTheme theme)
    {
        if (!IsSupported) return null;
        var instance = GetModuleHandle(null);
        if (!_classRegistered)
        {
            var windowClass = new WindowClass
            {
                Size = (uint)Marshal.SizeOf<WindowClass>(),
                Procedure = Marshal.GetFunctionPointerForDelegate(Procedure),
                Instance = instance,
                ClassName = ClassName
            };
            if (RegisterClassEx(ref windowClass) == 0)
            {
                Trace.TraceWarning("Dock backdrop class registration failed: {0}", Marshal.GetLastWin32Error());
                return null;
            }
            _classRegistered = true;
        }

        var handle = CreateWindowEx(WsExToolWindow | WsExNoActivate, ClassName, "OpenTaskbar dock backdrop", WsPopup,
            -32000, -32000, 1, 1, 0, 0, instance, 0);
        if (handle == 0)
        {
            Trace.TraceWarning("Dock backdrop window creation failed: {0}", Marshal.GetLastWin32Error());
            return null;
        }

        var backdrop = new DockAcrylicBackdrop(handle, dockHandle, theme);
        if (!backdrop.ApplyMaterial())
        {
            DestroyWindow(handle);
            return null;
        }

        AppLog.Info("Dock acrylic backdrop enabled.");
        return backdrop;
    }

    /// <summary>Paints nothing (the extended frame shows the blur) and lets clicks fall through.</summary>
    private static nint BackdropProcedure(nint window, uint message, nint wParam, nint lParam)
    {
        switch (message)
        {
            case WmEraseBackground:
                return 1;
            case WmPaint:
                ValidateRect(window, 0);
                return 0;
            case WmNcHitTest:
                return HitTransparent;
            default:
                return DefWindowProc(window, message, wParam, lParam);
        }
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

        var placement = (visibleLeft, visibleTop, visibleRight - visibleLeft, visibleBottom - visibleTop);
        // Layout passes call this often; only touch the window when it moved or the dock changed its z-order.
        if (!restack && _placed == placement) return;

        // Match the dock's band (topmost or not) first; a window is only placed right below another in its own band.
        var dockTopmost = (GetWindowLongPtr(_dockHandle, GwlExStyle) & WsExTopmost) != 0;
        var ownTopmost = (GetWindowLongPtr(_handle, GwlExStyle) & WsExTopmost) != 0;
        if (dockTopmost != ownTopmost)
            SetWindowPos(_handle, dockTopmost ? HwndTopmost : HwndNoTopmost, 0, 0, 0, 0, SwpNoMove | SwpNoSize | SwpNoActivate | SwpNoOwnerZOrder);

        // Nothing is painted in this window, so skip redrawing and copying its bits on every move.
        SetWindowPos(_handle, _dockHandle, placement.Item1, placement.Item2, placement.Item3, placement.Item4,
            SwpNoActivate | SwpNoOwnerZOrder | SwpNoRedraw | SwpNoCopyBits | SwpShowWindow);
        _placed = placement;
    }

    private void Hide()
    {
        if (_placed is null) return;
        _placed = null;
        SetWindowPos(_handle, 0, 0, 0, 0, 0, SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpNoOwnerZOrder | SwpHideWindow);
    }

    public void Dispose()
    {
        _placed = null;
        DestroyWindow(_handle);
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

    private delegate nint WindowProcedure(nint window, uint message, nint wParam, nint lParam);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowClass
    {
        public uint Size;
        public uint Style;
        public nint Procedure;
        public int ClassExtra;
        public int WindowExtra;
        public nint Instance;
        public nint Icon;
        public nint Cursor;
        public nint Background;
        public string? MenuName;
        public string ClassName;
        public nint SmallIcon;
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

    [StructLayout(LayoutKind.Sequential)]
    private struct Margins(int value)
    {
        public int Left = value;
        public int Right = value;
        public int Top = value;
        public int Bottom = value;
    }

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode, EntryPoint = "RegisterClassExW")]
    private static extern ushort RegisterClassEx(ref WindowClass windowClass);

    [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode, EntryPoint = "CreateWindowExW")]
    private static extern nint CreateWindowEx(int exStyle, string className, string windowName, uint style,
        int x, int y, int width, int height, nint parent, nint menu, nint instance, nint parameter);

    [DllImport("user32.dll", EntryPoint = "DefWindowProcW")]
    private static extern nint DefWindowProc(nint window, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyWindow(nint window);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ValidateRect(nint window, nint rect);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetModuleHandleW")]
    private static extern nint GetModuleHandle(string? moduleName);

    [DllImport("user32.dll")]
    private static extern int SetWindowCompositionAttribute(nint window, ref CompositionAttributeData data);

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(nint window, uint attribute, ref int value, uint size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(nint window, ref Margins margins);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr(nint window, int index);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint window, nint insertAfter, int x, int y, int width, int height, uint flags);
}
