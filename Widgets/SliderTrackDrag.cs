using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;

namespace OpenTaskbar.Widgets;

/// <summary>
/// Lets a slider be dragged from any point on its track. With <see cref="Slider.IsMoveToPointEnabled"/> a press on the
/// track only jumps the value there; this then hands the same press to the thumb, so moving the pointer keeps dragging.
/// </summary>
public static class SliderTrackDrag
{
    public static readonly DependencyProperty IsEnabledProperty = DependencyProperty.RegisterAttached(
        "IsEnabled", typeof(bool), typeof(SliderTrackDrag), new PropertyMetadata(false, OnIsEnabledChanged));

    private static readonly MouseButtonEventHandler PressHandler = Slider_PreviewMouseLeftButtonDown;

    public static bool GetIsEnabled(DependencyObject element) => (bool)element.GetValue(IsEnabledProperty);

    public static void SetIsEnabled(DependencyObject element, bool value) => element.SetValue(IsEnabledProperty, value);

    private static void OnIsEnabledChanged(DependencyObject element, DependencyPropertyChangedEventArgs e)
    {
        if (element is not Slider slider) return;
        slider.RemoveHandler(UIElement.PreviewMouseLeftButtonDownEvent, PressHandler);
        // The slider's own press handling (the jump) runs first and marks the press handled.
        if ((bool)e.NewValue) slider.AddHandler(UIElement.PreviewMouseLeftButtonDownEvent, PressHandler, handledEventsToo: true);
    }

    private static void Slider_PreviewMouseLeftButtonDown(object sender, MouseButtonEventArgs e)
    {
        if (sender is not Slider { IsMoveToPointEnabled: true } slider
            || slider.Template?.FindName("PART_Track", slider) is not Track { Thumb: { } thumb }
            || thumb.IsMouseOver)
        {
            return;
        }

        // The slider has already jumped the value to the pointer. Lay the thumb out there first: it measures the drag
        // from where it is drawn, so a thumb still at the old value would pull the value back on the first move.
        slider.UpdateLayout();
        thumb.RaiseEvent(new MouseButtonEventArgs(e.MouseDevice, e.Timestamp, MouseButton.Left)
        {
            RoutedEvent = UIElement.MouseLeftButtonDownEvent,
            Source = thumb
        });
        e.Handled = true;
    }
}
