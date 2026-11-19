using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Media.Animation;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>
/// Live reordering: while an item is dragged along the dock it follows the pointer and its neighbours slide aside to
/// open the slot it will land in. The saved order still changes only on drop, through <see cref="MoveDockItem"/>; the
/// slide uses the same insertion index, so what the dock shows is where the item lands.
/// </summary>
public partial class MainWindow
{
    private static readonly Duration LiveDragSlide = new(TimeSpan.FromMilliseconds(160));
    private LiveDockDrag? _liveDrag;

    private sealed record LiveDockSlot(FrameworkElement Element, double Start, double Size, TranslateTransform Translate);

    private sealed class LiveDockDrag(string token, IReadOnlyList<LiveDockSlot> slots, int draggedIndex, double origin, int zIndex)
    {
        public string Token { get; } = token;
        public IReadOnlyList<LiveDockSlot> Slots { get; } = slots;
        public int DraggedIndex { get; } = draggedIndex;
        public LiveDockSlot Dragged => Slots[DraggedIndex];
        public double Origin { get; } = origin;
        public int ZIndex { get; } = zIndex;
        public int InsertionIndex { get; set; } = -1;
        public double Offset { get; set; }
    }

    private DependencyProperty LiveDragAxis => IsVertical ? TranslateTransform.YProperty : TranslateTransform.XProperty;

    private bool LiveDragAnimates => _preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation;

    private void InitializeLiveDrag()
    {
        // Items mark their own DragOver as handled, so listen to handled events too.
        DockContents.AddHandler(DragOverEvent, new DragEventHandler(DockContents_LiveDragOver), handledEventsToo: true);
        DockContents.AddHandler(DragLeaveEvent, new DragEventHandler(DockContents_LiveDragLeave), handledEventsToo: true);
    }

    private double PointerAlongDock(DragEventArgs e)
    {
        var point = e.GetPosition(DockContents);
        return IsVertical ? point.Y : point.X;
    }

    private void DockContents_LiveDragOver(object sender, DragEventArgs e)
    {
        if (!TryGetDockItemToken(e, out var token)) return;
        var pointer = PointerAlongDock(e);
        if (_liveDrag is null) BeginLiveDrag(token, pointer);
        if (_liveDrag is { } drag && string.Equals(drag.Token, token, StringComparison.OrdinalIgnoreCase))
            UpdateLiveDrag(drag, pointer);
    }

    private void DockContents_LiveDragLeave(object sender, DragEventArgs e)
    {
        if (_liveDrag is not { } drag) return;
        // DragLeave also fires when the pointer crosses from one item to the next.
        if (new Rect(DockContents.RenderSize).Contains(e.GetPosition(DockContents))) return;
        // Off the dock: close the gap and return the item to its slot until the pointer comes back.
        drag.InsertionIndex = -1;
        drag.Offset = 0;
        foreach (var slot in drag.Slots) Slide(slot.Translate, 0);
    }

    private void BeginLiveDrag(string token, double pointer)
    {
        if (!_dockItemElements.TryGetValue(token, out var dragged)) return;
        // Layout slots ignore render transforms, so the lift and scale of the dragged item do not skew them.
        var slots = VisibleDockItems()
            .Select(item => (Element: item.Value, Slot: LayoutInformation.GetLayoutSlot(item.Value)))
            .OrderBy(item => IsVertical ? item.Slot.Y : item.Slot.X)
            .Select(item => new LiveDockSlot(item.Element,
                IsVertical ? item.Slot.Y : item.Slot.X,
                IsVertical ? item.Slot.Height : item.Slot.Width,
                AttachLiveTranslate(item.Element)))
            .ToList();
        var draggedIndex = slots.FindIndex(slot => ReferenceEquals(slot.Element, dragged));
        if (draggedIndex < 0) return;

        _liveDrag = new LiveDockDrag(token, slots, draggedIndex, pointer, Panel.GetZIndex(dragged));
        // Draw the dragged item above the neighbours it passes over.
        Panel.SetZIndex(dragged, 100);
    }

    private void UpdateLiveDrag(LiveDockDrag drag, double pointer)
    {
        var dragged = drag.Dragged;
        var extent = IsVertical ? DockContents.ActualHeight : DockContents.ActualWidth;
        drag.Offset = Math.Clamp(pointer - drag.Origin, -dragged.Start, Math.Max(-dragged.Start, extent - dragged.Start - dragged.Size));
        Place(dragged.Translate, drag.Offset);

        var index = DockItemOrdering.InsertionIndex(drag.Slots.Select(slot => (slot.Start, slot.Size)).ToList(), pointer);
        if (index == drag.InsertionIndex) return;
        drag.InsertionIndex = index;
        for (var i = 0; i < drag.Slots.Count; i++)
        {
            if (i == drag.DraggedIndex) continue;
            Slide(drag.Slots[i].Translate, DockItemOrdering.SlideFor(i, drag.DraggedIndex, index, dragged.Size));
        }
    }

    /// <summary>The drop index for this drop: the live index while a live drag is shown, so the item lands where the gap opened.</summary>
    private int DropInsertionIndex(DragEventArgs e) => _liveDrag is { } drag
        ? DockItemOrdering.InsertionIndex(drag.Slots.Select(slot => (slot.Start, slot.Size)).ToList(), PointerAlongDock(e))
        : GetDockInsertionIndex(e);

    /// <summary>Drops a dragged dock item: moves it, then lets it glide from the pointer into its new slot.</summary>
    private void DropDockItem(string token, DragEventArgs e)
    {
        var index = DropInsertionIndex(e);
        var drag = _liveDrag;
        _liveDrag = null;
        if (drag is null)
        {
            MoveDockItem(token, index);
            return;
        }

        var shownAt = drag.Dragged.Start + drag.Offset;
        Panel.SetZIndex(drag.Dragged.Element, drag.ZIndex);
        // The neighbours already stand where the new order puts them; drop their offsets in the same frame as the move.
        foreach (var slot in drag.Slots) Place(slot.Translate, 0);
        MoveDockItem(token, index);
        DockContents.UpdateLayout();
        if (!_dockItemElements.TryGetValue(token, out var landed)) return;
        var landedSlot = LayoutInformation.GetLayoutSlot(landed);
        var translate = AttachLiveTranslate(landed);
        Place(translate, shownAt - (IsVertical ? landedSlot.Y : landedSlot.X));
        Slide(translate, 0);
    }

    /// <summary>Ends a drag that was not dropped on the dock (Esc, or released elsewhere): everything slides back.</summary>
    private void CancelLiveDrag()
    {
        if (_liveDrag is not { } drag) return;
        _liveDrag = null;
        Panel.SetZIndex(drag.Dragged.Element, drag.ZIndex);
        foreach (var slot in drag.Slots) Slide(slot.Translate, 0);
    }

    private void Place(TranslateTransform translate, double value)
    {
        translate.BeginAnimation(LiveDragAxis, null);
        translate.SetValue(LiveDragAxis, value);
    }

    private void Slide(TranslateTransform translate, double to)
    {
        if (!LiveDragAnimates)
        {
            Place(translate, to);
            return;
        }

        translate.BeginAnimation(LiveDragAxis, new DoubleAnimation(to, LiveDragSlide)
        {
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut }
        });
    }

    /// <summary>
    /// The element's own translate when it has one (a widget's lift transform), otherwise a new one composed with
    /// its current transform. It stays on the element at zero after the drag.
    /// </summary>
    private static TranslateTransform AttachLiveTranslate(FrameworkElement element)
    {
        switch (element.RenderTransform)
        {
            case TranslateTransform { IsFrozen: false } own:
                return own;
            case TransformGroup { IsFrozen: false } group when group.Children.OfType<TranslateTransform>().FirstOrDefault(child => !child.IsFrozen) is { } child:
                return child;
        }

        var translate = new TranslateTransform();
        var current = element.RenderTransform;
        // Only a frozen identity is replaced; a live transform (a hover scale, say) is kept and composed.
        element.RenderTransform = current is null || (current.IsFrozen && current.Value.IsIdentity)
            ? translate
            : new TransformGroup { Children = { current, translate } };
        return translate;
    }
}
