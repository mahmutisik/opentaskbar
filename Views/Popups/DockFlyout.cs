using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Documents;
using System.Windows.Input;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>
/// A glass popup anchored to a dock element (stacks, calendar, tray, desktops). Opens away from the docked screen edge
/// and closes when the pointer clicks elsewhere.
/// </summary>
public sealed class DockFlyout
{
    private static readonly SolidColorBrush ShadowBrush1 = FreezeBrush(Color.FromArgb(0x26, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush2 = FreezeBrush(Color.FromArgb(0x18, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush3 = FreezeBrush(Color.FromArgb(0x0E, 0x05, 0x07, 0x0B));
    private static readonly SolidColorBrush ShadowBrush4 = FreezeBrush(Color.FromArgb(0x06, 0x05, 0x07, 0x0B));

    private static SolidColorBrush FreezeBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    internal static void AttachSoftShadow(Panel host, double cornerRadius)
    {
        // Four concentric outer stroke rings with transparent interiors preserve glass translucency without ShaderEffect/D3DCompiler.
        AddRing(host, 2, 1, cornerRadius + 2, ShadowBrush1);
        AddRing(host, 5, 2, cornerRadius + 5, ShadowBrush2);
        AddRing(host, 9, 3, cornerRadius + 9, ShadowBrush3);
        AddRing(host, 14, 4, cornerRadius + 14, ShadowBrush4);
    }

    private static void AddRing(Panel host, double spread, double yOffset, double radius, Brush brush)
    {
        var prev = spread switch
        {
            2 => 0d,
            5 => 2d,
            9 => 5d,
            _ => 9d
        };
        var thickness = spread - prev;
        host.Children.Add(new Border
        {
            Margin = new Thickness(-spread, -spread + yOffset, -spread, -spread - yOffset),
            CornerRadius = new CornerRadius(radius),
            BorderThickness = new Thickness(thickness),
            BorderBrush = brush,
            Background = Brushes.Transparent,
            IsHitTestVisible = false
        });
    }

    private static readonly List<WeakReference<DockFlyout>> FlyoutRegistry = [];
    public static event Action? AnyClosed;
    private UIElement? _currentTarget;
    private bool _isOpenState;
    private bool _suppressNextClick;
    private bool _explicitClose;
    private WeakReference<UIElement>? _closedTarget;
    private long _closedAt;
    private nint _previousForeground;
    private bool _restoreFocus;

    private readonly Popup _popup;
    private readonly Grid _root;
    private readonly Border _frame;
    private readonly Border _innerBevel;

    private static void CleanRegistry() => FlyoutRegistry.RemoveAll(r => !r.TryGetTarget(out _));

    public static bool IsAnyOpen
    {
        get
        {
            CleanRegistry();
            for (var i = 0; i < FlyoutRegistry.Count; i++)
            {
                if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.IsOpen)
                    return true;
            }
            return false;
        }
    }

    public static bool IsMouseOverAny
    {
        get
        {
            CleanRegistry();
            for (var i = 0; i < FlyoutRegistry.Count; i++)
            {
                if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.IsOpen)
                {
                    if (flyout._root.IsMouseOver || flyout._frame.IsMouseOver)
                        return true;
                }
            }
            return false;
        }
    }

    public static void CloseAll(bool restoreFocus = true)
    {
        CleanRegistry();
        for (var i = 0; i < FlyoutRegistry.Count; i++)
        {
            if (FlyoutRegistry[i].TryGetTarget(out var flyout) && flyout._popup.IsOpen)
                flyout.Close(restoreFocus);
        }
    }

    public DockFlyout()
    {
        FlyoutRegistry.Add(new WeakReference<DockFlyout>(this));
        _innerBevel = new Border
        {
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(13),
            Padding = new Thickness(12),
            Background = System.Windows.Media.Brushes.Transparent
        };
        _innerBevel.SetResourceReference(Border.BorderBrushProperty, "DockInnerBevelMaterial");

        _frame = new Border
        {
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(14),
            Padding = new Thickness(0),
            Child = _innerBevel
        };
        _frame.SetResourceReference(Border.BackgroundProperty, "MenuMaterial");
        _frame.SetResourceReference(Border.BorderBrushProperty, "MenuStroke");
        _frame.SetResourceReference(TextElement.ForegroundProperty, "TextPrimary");
        _frame.SetResourceReference(TextElement.FontFamilyProperty, "InterfaceFont");

        var contentHost = new Grid();
        AttachSoftShadow(contentHost, 14);
        contentHost.Children.Add(_frame);

        _root = new Grid { Margin = new Thickness(12, 10, 12, 14), Children = { contentHost } };
        _popup = new Popup
        {
            AllowsTransparency = true,
            StaysOpen = false,
            Child = _root,
            PopupAnimation = PopupAnimation.Fade,
            Placement = PlacementMode.Custom,
            CustomPopupPlacementCallback = Place
        };
        _popup.Closed += (_, _) =>
        {
            _isOpenState = false;
            var target = _currentTarget ?? _popup.PlacementTarget;
            var isClosingTargetClick = !_explicitClose && target is not null && (_suppressNextClick || IsCursorOver(target));
            if (target is not null)
            {
                _closedTarget = new WeakReference<UIElement>(target);
            }
            if (_currentTarget is not null)
            {
                _currentTarget.PreviewMouseDown -= Target_PreviewMouseDown;
                _currentTarget = null;
            }
            _closedAt = Environment.TickCount64;
            _suppressNextClick = isClosingTargetClick;
            _explicitClose = false;
            // Light dismiss and Esc hand the keyboard back to the app that had it before the flyout opened.
            if (_restoreFocus && _previousForeground != 0)
            {
                NativeWindowTracker.Activate(_previousForeground);
            }
            _restoreFocus = false;
            _previousForeground = 0;

            _innerBevel.Child = null;
            _popup.PlacementTarget = null;
            Keyboard.ClearFocus();
            Closed?.Invoke(this, EventArgs.Empty);
            AnyClosed?.Invoke();
        };
        _popup.KeyDown += (_, args) =>
        {
            if (args.Key != System.Windows.Input.Key.Escape) return;
            args.Handled = true;
            _popup.IsOpen = false;
        };
    }

    /// <summary>The dock edge the flyouts open away from.</summary>
    public static DockEdge Edge { get; set; } = DockEdge.Bottom;

    public event EventHandler? Closed;

    public bool IsOpen => _isOpenState || _popup.IsOpen;

    private void Target_PreviewMouseDown(object sender, MouseButtonEventArgs e)
    {
        AppLog.Info($"[DockFlyout.PreviewMouseDown] sender={sender.GetType().Name}, IsOpen={_popup.IsOpen}");
        if (_popup.IsOpen)
        {
            _suppressNextClick = true;
            _closedTarget = new WeakReference<UIElement>((UIElement)sender);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint pt);

    private static bool IsCursorOver(UIElement? element)
    {
        if (element is null || !element.IsVisible) return false;
        if (element.IsMouseOver) return true;
        if (element is FrameworkElement fe && TryGetScreenBounds(fe, padding: 6, out var bounds))
        {
            if (GetCursorPos(out var pt))
            {
                return pt.X >= bounds.Left && pt.X <= bounds.Right && pt.Y >= bounds.Top && pt.Y <= bounds.Bottom;
            }
        }
        return false;
    }

    private static bool TryGetScreenBounds(FrameworkElement element, double padding, out (double Left, double Top, double Right, double Bottom) bounds)
    {
        bounds = default;
        var source = PresentationSource.FromVisual(element);
        if (source?.CompositionTarget is null || element.ActualWidth <= 0 || element.ActualHeight <= 0)
        {
            return false;
        }

        try
        {
            var dpiX = source.CompositionTarget.TransformToDevice.M11;
            var dpiY = source.CompositionTarget.TransformToDevice.M22;
            var padX = padding * dpiX;
            var padY = padding * dpiY;
            var tl = element.PointToScreen(new Point(0, 0));
            var br = element.PointToScreen(new Point(element.ActualWidth, element.ActualHeight));
            bounds = (Math.Min(tl.X, br.X) - padX, Math.Min(tl.Y, br.Y) - padY, Math.Max(tl.X, br.X) + padX, Math.Max(tl.Y, br.Y) + padY);
            return true;
        }
        catch (Exception ex)
        {
            AppLog.Warning($"[TryGetScreenBounds] PointToScreen error: {ex.Message}");
            return false;
        }
    }

    /// <param name="lightDismiss">
    /// False for hover cards: the popup then takes no mouse capture, so a click on the widget under it reaches the
    /// widget instead of only closing the card. The caller closes it when the pointer leaves.
    /// </param>
    public void Show(UIElement target, UIElement content, bool animate = true, bool focusable = false, bool lightDismiss = true)
    {
        _isOpenState = true;
        _explicitClose = false;
        _suppressNextClick = false;
        _closedTarget = null;
        _root.Margin = Edge switch
        {
            DockEdge.Left => new Thickness(12, 10, 14, 14),
            DockEdge.Right => new Thickness(14, 10, 12, 14),
            _ => new Thickness(12, 10, 12, 14)
        };
        _innerBevel.Child = content;
        _popup.StaysOpen = !lightDismiss;
        _popup.PopupAnimation = animate && SystemParameters.ClientAreaAnimation ? PopupAnimation.Fade : PopupAnimation.None;
        CleanRegistry();
        for (var i = 0; i < FlyoutRegistry.Count; i++)
        {
            if (FlyoutRegistry[i].TryGetTarget(out var flyout) && !ReferenceEquals(flyout, this) && flyout._popup.IsOpen)
            {
                flyout.Close();
            }
        }
        if (_currentTarget is not null && !ReferenceEquals(_currentTarget, target))
        {
            _currentTarget.PreviewMouseDown -= Target_PreviewMouseDown;
        }
        _currentTarget = target;
        target.PreviewMouseDown -= Target_PreviewMouseDown;
        target.PreviewMouseDown += Target_PreviewMouseDown;
        _popup.PlacementTarget = target;
        if (focusable)
        {
            var owner = Window.GetWindow(target);
            _previousForeground = NativeWindowTracker.GetForegroundApplicationWindow();
            _restoreFocus = true;
            owner?.Activate();
            _popup.IsOpen = true;
            _frame.Focusable = true;
            _frame.Focus();
        }
        else
        {
            _previousForeground = 0;
            _restoreFocus = false;
            _popup.IsOpen = true;
            _frame.Focusable = false;
        }
    }

    public void Toggle(UIElement target, Func<UIElement> content, bool animate = true, bool focusable = false)
    {
        var isAnchorMatch = ReferenceEquals(_currentTarget, target) || ReferenceEquals(_popup.PlacementTarget, target);
        if (isAnchorMatch && (_isOpenState || _popup.IsOpen))
        {
            _isOpenState = false;
            _suppressNextClick = false;
            _closedTarget = null;
            Close();
            return;
        }

        if (_suppressNextClick && (_closedTarget is null || (_closedTarget.TryGetTarget(out var prev) && ReferenceEquals(prev, target))))
        {
            _suppressNextClick = false;
            _closedTarget = null;
            return;
        }

        Show(target, content(), animate, focusable);
    }

    public bool RecentlyClosed(UIElement target)
    {
        if (_suppressNextClick && (_closedTarget is null || (_closedTarget.TryGetTarget(out var clickTarget) && ReferenceEquals(clickTarget, target))))
        {
            _suppressNextClick = false;
            _closedTarget = null;
            return true;
        }

        if (_closedTarget is not null && _closedTarget.TryGetTarget(out var prev) && ReferenceEquals(prev, target)
            && Environment.TickCount64 - _closedAt < 350)
        {
            return true;
        }

        return false;
    }

    /// <summary>Closes flyout; clears focus restoration when explicitly requested.</summary>
    public void Close(bool restoreFocus = true)
    {
        _explicitClose = true;
        _isOpenState = false;
        if (!restoreFocus) _restoreFocus = false;
        _popup.IsOpen = false;
    }

    private const double FlyoutGap = 10;

    private static CustomPopupPlacement[] Place(Size popupSize, Size targetSize, Point offset)
    {
        var point = Edge switch
        {
            DockEdge.Left => new Point(targetSize.Width + FlyoutGap, (targetSize.Height - popupSize.Height) / 2),
            DockEdge.Right => new Point(-popupSize.Width - FlyoutGap, (targetSize.Height - popupSize.Height) / 2),
            _ => new Point((targetSize.Width - popupSize.Width) / 2, -popupSize.Height - FlyoutGap)
        };
        return [new CustomPopupPlacement(point, PopupPrimaryAxis.None)];
    }

    /// <summary>Flat, borderless button used for flyout rows and tiles.</summary>
    public static Button FlatButton(object content, RoutedEventHandler click, string? toolTip = null)
    {
        var button = new Button { Content = content, ToolTip = toolTip, Cursor = System.Windows.Input.Cursors.Hand };
        button.SetResourceReference(FrameworkElement.StyleProperty, "FlyoutButton");
        button.Click += click;
        if (toolTip is not null) System.Windows.Automation.AutomationProperties.SetName(button, toolTip);
        return button;
    }

    public static TextBlock Heading(string text)
    {
        var block = new TextBlock { Text = text, FontWeight = FontWeights.SemiBold, FontSize = 13, Margin = new Thickness(4, 0, 4, 8) };
        return block;
    }

    public static TextBlock Caption(string text)
    {
        var block = new TextBlock { Text = text, FontSize = 11.5, TextWrapping = TextWrapping.Wrap, Margin = new Thickness(4, 2, 4, 2) };
        block.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        return block;
    }
}
