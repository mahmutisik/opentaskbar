namespace OpenTaskbar.Services;

/// <summary>Converts a drop position in the visible dock into the saved item order.</summary>
internal static class DockItemOrdering
{
    /// <summary>
    /// Drop index for a pointer position along the dock: the number of visible items, the dragged one included,
    /// whose center lies before the pointer. This is the index <see cref="MoveVisibleItem"/> expects.
    /// </summary>
    internal static int InsertionIndex(IReadOnlyList<(double Start, double Size)> slots, double pointer)
    {
        var index = 0;
        foreach (var (start, size) in slots)
        {
            if (pointer > start + size / 2) index++;
            else break;
        }
        return index;
    }

    /// <summary>How far an item slides aside while another is dragged, so the dragged item's landing slot opens.</summary>
    internal static double SlideFor(int item, int dragged, int insertionIndex, double draggedSize)
    {
        if (item == dragged) return 0;
        var destination = insertionIndex > dragged ? insertionIndex - 1 : insertionIndex;
        if (item > dragged && item <= destination) return -draggedSize;
        if (item < dragged && item >= destination) return draggedSize;
        return 0;
    }

    internal static IReadOnlyList<string> MoveVisibleItem(
        IReadOnlyList<string> savedOrder,
        IReadOnlyList<string> visibleOrder,
        string movedToken,
        int insertionIndex)
    {
        var comparer = StringComparer.OrdinalIgnoreCase;
        var visible = visibleOrder.Distinct(comparer).ToList();
        var fromIndex = visible.FindIndex(token => comparer.Equals(token, movedToken));
        if (fromIndex < 0) return savedOrder.ToArray();

        // The drop index includes the dragged item, but the insertion list does not.
        var destination = Math.Clamp(insertionIndex, 0, visible.Count);
        if (fromIndex < destination) destination--;

        var source = visible[fromIndex];
        visible.RemoveAt(fromIndex);
        visible.Insert(destination, source);

        // Keep hidden/inactive items in their saved slots instead of counting them as
        // visible drop targets. They should reappear in their previous relative order.
        var visibleSet = new HashSet<string>(visibleOrder, comparer);
        var result = new List<string>(savedOrder.Count + visible.Count);
        var seen = new HashSet<string>(comparer);
        var nextVisible = 0;
        foreach (var token in savedOrder)
        {
            if (!seen.Add(token)) continue;
            if (visibleSet.Contains(token))
            {
                if (nextVisible < visible.Count) result.Add(visible[nextVisible++]);
            }
            else
            {
                result.Add(token);
            }
        }

        while (nextVisible < visible.Count) result.Add(visible[nextVisible++]);
        return result;
    }
}
