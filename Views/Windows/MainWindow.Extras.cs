using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>Dock items after the apps: folder stacks and the Recycle Bin.</summary>
public partial class MainWindow
{
    private const int StackEntryLimit = 16;
    private readonly ObservableCollection<DockStack> _stacks = [];
    private readonly DockFlyout _stackFlyout = new();
    private DispatcherTimer? _recycleBinTimer;
    private bool? _recycleBinFull;
    private bool _recycleBinQueryRunning;
    private bool _recycleBinQueryAgain;
    private Point _dragStackStartPoint;
    private DockStack? _draggedStack;

    private static string DownloadsFolder => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");

    private void InitializeExtras()
    {
        StackItems.ItemsSource = _stacks;
        _stackFlyout.Closed += (_, _) => _autoHideController?.EvaluateState();
        _recycleBinTimer = new DispatcherTimer(DispatcherPriority.Background, Dispatcher) { Interval = TimeSpan.FromSeconds(3) };
        _recycleBinTimer.Tick += (_, _) => UpdateRecycleBin();
        ApplyExtrasSettings();
    }

    private static readonly Brush StackTileBrush = CreateFrozenStackBrush(Color.FromArgb(0x35, 0xFF, 0xFF, 0xFF));
    private static readonly Brush StackTileStroke = CreateFrozenStackBrush(Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardShadowBrush1 = CreateFrozenStackBrush(Color.FromArgb(0x35, 0x00, 0x00, 0x00));
    private static readonly Brush CardShadowBrush2 = CreateFrozenStackBrush(Color.FromArgb(0x18, 0x00, 0x00, 0x00));
    private static readonly Brush CardDeckBackBrush = CreateFrozenStackBrush(Color.FromArgb(0x28, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckBackStroke = CreateFrozenStackBrush(Color.FromArgb(0x38, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckMidBrush = CreateFrozenStackBrush(Color.FromArgb(0x3C, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckMidStroke = CreateFrozenStackBrush(Color.FromArgb(0x50, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckFrontBrush = CreateFrozenStackBrush(Color.FromArgb(0x50, 0xFF, 0xFF, 0xFF));
    private static readonly Brush CardDeckFrontStroke = CreateFrozenStackBrush(Color.FromArgb(0x75, 0xFF, 0xFF, 0xFF));
    private static readonly Brush BadgeBackdropBrush = CreateFrozenStackBrush(Color.FromArgb(0xE0, 0x1E, 0x29, 0x3B));
    private static readonly Brush BadgeStrokeBrush = CreateFrozenStackBrush(Color.FromArgb(0x60, 0xFF, 0xFF, 0xFF));
    private static readonly Brush BadgeIconBrush = CreateFrozenStackBrush(Color.FromArgb(0xF0, 0x60, 0xA5, 0xFA));

    private static Brush CreateFrozenStackBrush(Color color)
    {
        var brush = new SolidColorBrush(color);
        brush.Freeze();
        return brush;
    }

    private int StackIconPixels => ApplicationIconProvider.ComputeTargetPixels(Profile.Icon, VisualTreeHelper.GetDpi(this).DpiScaleX);

    private void ApplyExtrasSettings()
    {
        var showBin = _preferences.Current.ShowRecycleBin;
        RecycleBinButton.Visibility = showBin ? Visibility.Visible : Visibility.Collapsed;
        if (showBin)
        {
            UpdateRecycleBin();
            _recycleBinTimer?.Start();
        }
        else
        {
            _recycleBinTimer?.Stop();
        }

        var folders = ConfiguredStackFolders();
        if (!folders.SequenceEqual(_stacks.Select(stack => stack.Path), StringComparer.OrdinalIgnoreCase))
        {
            foreach (var stack in _stacks) stack.Dispose();
            _stacks.Clear();
            foreach (var folder in folders.Where(Directory.Exists))
            {
                var stack = new DockStack(folder, Dispatcher);
                stack.Icon = CreateStackIcon(stack, StackIconPixels);
                stack.ContentsChanged += () => stack.Icon = CreateStackIcon(stack, StackIconPixels);
                _stacks.Add(stack);
            }
        }

        ExtrasDivider.Visibility = (showBin || _stacks.Count > 0) && (VisiblePinnedCount > 0 || _runningApps.Count > 0)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private void UpdateExtrasDividerVisibility()
    {
        ExtrasDivider.Visibility = (RecycleBinButton.Visibility == Visibility.Visible || _stacks.Count > 0)
            && (VisiblePinnedCount > 0 || _runningApps.Count > 0)
            ? Visibility.Visible : Visibility.Collapsed;
    }

    private IReadOnlyList<string> ConfiguredStackFolders() => _preferences.Current.StackFolders ?? [DownloadsFolder];

    private int ExtraButtonCount => _stacks.Count + (RecycleBinButton.Visibility == Visibility.Visible ? 1 : 0);

    private static readonly Geometry FolderBadgeGeometry = CreateFrozenGeometry("M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z");

    private static Geometry CreateFrozenGeometry(string pathData)
    {
        var geom = Geometry.Parse(pathData);
        geom.Freeze();
        return geom;
    }

    internal static BitmapSource? CreateStackIcon(DockStack stack, int targetSize = 128)
    {
        var size = Math.Max(16, targetSize);
        var entries = stack.RecentEntries(3);
        if (entries.Count == 0)
        {
            return ApplicationIconProvider.GetForShellItem(stack.Path, size, iconOnly: true);
        }

        var scale = size / 128.0;

        var visual = new DrawingVisual();
        using (var dc = visual.RenderOpen())
        {
            // 1. Back card (top-right depth)
            var rectBack = new Rect(24 * scale, 8 * scale, 76 * scale, 76 * scale);
            var cornerBack = 14 * scale;
            dc.DrawRoundedRectangle(CardShadowBrush2, null, new Rect(rectBack.X, rectBack.Y + 3 * scale, rectBack.Width, rectBack.Height), cornerBack, cornerBack);
            var penBack = new Pen(CardDeckBackStroke, Math.Max(0.5, 1.2 * scale));
            penBack.Freeze();
            dc.DrawRoundedRectangle(CardDeckBackBrush, penBack, rectBack, cornerBack, cornerBack);
            if (entries.Count >= 3)
            {
                var iconBack = ApplicationIconProvider.GetForShellItem(entries[2].FullName, (int)Math.Ceiling(44 * scale), iconOnly: true);
                if (iconBack is not null)
                {
                    dc.DrawImage(iconBack, new Rect(rectBack.X + (rectBack.Width - 44 * scale) / 2, rectBack.Y + (rectBack.Height - 44 * scale) / 2, 44 * scale, 44 * scale));
                }
            }

            // 2. Middle card (intermediate layer)
            var rectMid = new Rect(16 * scale, 20 * scale, 82 * scale, 82 * scale);
            var cornerMid = 15 * scale;
            dc.DrawRoundedRectangle(CardShadowBrush1, null, new Rect(rectMid.X, rectMid.Y + 3 * scale, rectMid.Width, rectMid.Height), cornerMid, cornerMid);
            var penMid = new Pen(CardDeckMidStroke, Math.Max(0.5, 1.4 * scale));
            penMid.Freeze();
            dc.DrawRoundedRectangle(CardDeckMidBrush, penMid, rectMid, cornerMid, cornerMid);
            if (entries.Count >= 2)
            {
                var iconMid = ApplicationIconProvider.GetForShellItem(entries[1].FullName, (int)Math.Ceiling(50 * scale), iconOnly: true);
                if (iconMid is not null)
                {
                    dc.DrawImage(iconMid, new Rect(rectMid.X + (rectMid.Width - 50 * scale) / 2, rectMid.Y + (rectMid.Height - 50 * scale) / 2, 50 * scale, 50 * scale));
                }
            }

            // 3. Front card (prominent foreground hero card)
            var rectFront = new Rect(8 * scale, 32 * scale, 88 * scale, 88 * scale);
            var cornerFront = 16 * scale;
            dc.DrawRoundedRectangle(CardShadowBrush1, null, new Rect(rectFront.X, rectFront.Y + 4 * scale, rectFront.Width, rectFront.Height), cornerFront, cornerFront);
            var penFront = new Pen(CardDeckFrontStroke, Math.Max(0.5, 1.6 * scale));
            penFront.Freeze();
            dc.DrawRoundedRectangle(CardDeckFrontBrush, penFront, rectFront, cornerFront, cornerFront);
            if (entries.Count >= 1)
            {
                var iconFront = ApplicationIconProvider.GetForShellItem(entries[0].FullName, (int)Math.Ceiling(58 * scale), iconOnly: true);
                if (iconFront is not null)
                {
                    dc.DrawImage(iconFront, new Rect(rectFront.X + (rectFront.Width - 58 * scale) / 2, rectFront.Y + (rectFront.Height - 58 * scale) / 2, 58 * scale, 58 * scale));
                }
            }

            // 4. Folder badge pill (bottom-right corner emblem)
            var badgeCenter = new Point(size - 22 * scale, size - 22 * scale);
            var badgeRadius = 18 * scale;
            dc.DrawEllipse(CardShadowBrush1, null, new Point(badgeCenter.X, badgeCenter.Y + 2 * scale), badgeRadius + 1 * scale, badgeRadius + 1 * scale);
            var badgePen = new Pen(BadgeStrokeBrush, Math.Max(0.5, 1.5 * scale));
            badgePen.Freeze();
            dc.DrawEllipse(BadgeBackdropBrush, badgePen, badgeCenter, badgeRadius, badgeRadius);

            var iconSize = 18 * scale;
            var scaleGeom = iconSize / 24.0;
            var originX = badgeCenter.X - (iconSize / 2);
            var originY = badgeCenter.Y - (iconSize / 2);
            dc.PushTransform(new TranslateTransform(originX, originY));
            dc.PushTransform(new ScaleTransform(scaleGeom, scaleGeom));
            var iconPen = new Pen(BadgeIconBrush, 2.0);
            iconPen.Freeze();
            dc.DrawGeometry(null, iconPen, FolderBadgeGeometry);
            dc.Pop();
            dc.Pop();
        }

        var rtb = new System.Windows.Media.Imaging.RenderTargetBitmap(size, size, 96, 96, PixelFormats.Pbgra32);
        rtb.Render(visual);
        rtb.Freeze();
        return rtb;
    }

    private IEnumerable<Button> GetExtraDockButtons()
    {
        foreach (var stack in _stacks)
        {
            if (StackItems.ItemContainerGenerator.ContainerFromItem(stack) is DependencyObject container
                && FindVisualChild<Button>(container) is { } button)
            {
                yield return button;
            }
        }

        if (RecycleBinButton.Visibility == Visibility.Visible) yield return RecycleBinButton;
    }

    /// <summary>
    /// Refreshes the Recycle Bin state off the UI thread. SHQueryRecycleBin walks the bin on every drive and takes
    /// around 100 ms inside the dock process; polled every 3 s on the UI thread it froze the whole dock each time.
    /// Requests that arrive while a query runs are coalesced into one follow-up query.
    /// </summary>
    private async void UpdateRecycleBin()
    {
        if (_recycleBinQueryRunning)
        {
            _recycleBinQueryAgain = true;
            return;
        }

        _recycleBinQueryRunning = true;
        try
        {
            do
            {
                _recycleBinQueryAgain = false;
                var count = await Task.Run(RecycleBin.ItemCount);
                ApplyRecycleBinCount(count);
            }
            while (_recycleBinQueryAgain);
        }
        finally
        {
            _recycleBinQueryRunning = false;
        }
    }

    private void ApplyRecycleBinCount(long? count)
    {
        var full = count > 0;
        if (_recycleBinFull == full) return;
        _recycleBinFull = full;
        RefreshRecycleBinIcon();
        EmptyRecycleBinItem.IsEnabled = full;
    }

    private void RefreshRecycleBinIcon()
    {
        var full = _recycleBinFull == true;
        RecycleBinIcon.Source = ThemePacks.ApplyIcon(full ? RecycleBin.FullIcon : RecycleBin.EmptyIcon, null,
            specialKey: full ? IconPack.RecycleBinFullKey : IconPack.RecycleBinKey);
    }

    private void RecycleBin_Click(object sender, RoutedEventArgs e)
    {
        _thumbnailPreview.Hide();
        if (sender is Button button) AnimateActivationFeedback(button);
        ReportResult(RecycleBin.Open());
    }

    private void EmptyRecycleBin_Click(object sender, RoutedEventArgs e)
    {
        ReportResult(RecycleBin.Empty(_source?.Handle ?? 0));
        _recycleBinFull = null;
        UpdateRecycleBin();
    }

    private void RecycleBin_DragOver(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out _))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }

        if (e.Data.GetDataPresent(typeof(StartButtonDragData)))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
            return;
        }

        e.Effects = e.Data.GetDataPresent(DataFormats.FileDrop) ? DragDropEffects.Move : DragDropEffects.None;
        e.Handled = true;
    }

    private void RecycleBin_Drop(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out var droppedToken))
        {
            DropDockItem(droppedToken, e);
            e.Handled = true;
            return;
        }

        e.Handled = true;
        if (e.Data.GetData(DataFormats.FileDrop) is not string[] { Length: > 0 } paths) return;
        // Let the drag source finish before Windows shows its own confirmation UI.
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            ReportResult(RecycleBin.Delete(paths, _source?.Handle ?? 0));
            _recycleBinFull = null;
            UpdateRecycleBin();
        });
    }

    private static void ReportResult(string? error)
    {
        if (error is not null) ReportError(error);
    }

    private void Stack_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button { DataContext: DockStack stack } button) return;
        _previewCloseTimer.Stop();
        _thumbnailPreview.Hide();
        ResetMagnification();
        stack.HasNewItems = false;
        _stackFlyout.Toggle(button, () => BuildStackContent(stack), _preferences.Current.AnimationsEnabled);
    }

    private UIElement BuildStackContent(DockStack stack)
    {
        var root = new StackPanel { Width = 4 * 96 };

        // 1. Sleek Header Bar with Folder Icon, Stack Name & Count Badge
        var header = new DockPanel { Margin = new Thickness(4, 2, 4, 10) };
        var entries = stack.RecentEntries(StackEntryLimit);

        var countBadge = new Border
        {
            CornerRadius = new CornerRadius(9),
            Padding = new Thickness(7, 2, 7, 2),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Center
        };
        countBadge.SetResourceReference(Border.BackgroundProperty, "SurfaceSelected");
        var countText = new TextBlock
        {
            Text = entries.Count.ToString(), FontSize = 11, FontWeight = FontWeights.SemiBold
        };
        countText.SetResourceReference(TextBlock.ForegroundProperty, "TextSecondary");
        countBadge.Child = countText;
        DockPanel.SetDock(countBadge, Dock.Right);
        header.Children.Add(countBadge);

        var titleGroup = new StackPanel { Orientation = Orientation.Horizontal, VerticalAlignment = VerticalAlignment.Center };
        var folderIcon = new ContentControl
        {
            Content = Application.Current?.TryFindResource("IconFolder"),
            Width = 15, Height = 15, Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        folderIcon.SetResourceReference(FrameworkElement.StyleProperty, "WidgetIcon");
        folderIcon.SetResourceReference(Control.ForegroundProperty, "WidgetAccent");

        var titleText = new TextBlock
        {
            Text = stack.Name, FontWeight = FontWeights.SemiBold, FontSize = 13.5,
            VerticalAlignment = VerticalAlignment.Center
        };
        titleText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        titleGroup.Children.Add(folderIcon);
        titleGroup.Children.Add(titleText);
        header.Children.Add(titleGroup);

        root.Children.Add(header);

        var divider = new System.Windows.Shapes.Rectangle
        {
            Height = 1, Margin = new Thickness(2, 0, 2, 10)
        };
        divider.SetResourceReference(System.Windows.Shapes.Shape.FillProperty, "StrokeSubtle");
        root.Children.Add(divider);

        if (entries.Count == 0)
        {
            root.Children.Add(DockFlyout.Caption(UiText.Get("StackEmpty")));
        }

        var flyoutIconPixels = ApplicationIconProvider.ComputeTargetPixels(48, VisualTreeHelper.GetDpi(this).DpiScaleX);
        var grid = new WrapPanel();
        foreach (var entry in entries)
        {
            var image = new Image { Width = 46, Height = 46, Margin = new Thickness(0, 4, 0, 6) };
            RenderOptions.SetBitmapScalingMode(image, BitmapScalingMode.HighQuality);
            var label = new TextBlock
            {
                Text = entry.Name, TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap,
                TextTrimming = TextTrimming.CharacterEllipsis, MaxHeight = 32, FontSize = 11
            };
            label.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");

            var tile = new StackPanel { Width = 84 };
            tile.Children.Add(image);
            tile.Children.Add(label);
            var path = entry.FullName;
            var button = DockFlyout.FlatButton(tile, (_, _) =>
            {
                _stackFlyout.Close();
                OpenPath(path);
            }, entry.Name);
            button.Padding = new Thickness(4, 6, 4, 6);
            button.Margin = new Thickness(2);
            grid.Children.Add(button);
            // Thumbnails can be slow (videos, network drives); fill them in after the flyout is shown.
            Dispatcher.BeginInvoke(DispatcherPriority.Background, () => image.Source = ApplicationIconProvider.GetForShellItem(path, flyoutIconPixels));
        }

        root.Children.Add(grid);

        // 2. Modern Footer Bar
        var footerPanel = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Center };
        var openFolderIcon = new ContentControl
        {
            Content = Application.Current?.TryFindResource("IconFolder"),
            Width = 14, Height = 14, Margin = new Thickness(0, 0, 8, 0),
            VerticalAlignment = VerticalAlignment.Center
        };
        openFolderIcon.SetResourceReference(FrameworkElement.StyleProperty, "WidgetIcon");
        openFolderIcon.SetResourceReference(Control.ForegroundProperty, "TextPrimary");

        var footerText = new TextBlock
        {
            Text = UiText.Get("OpenFolderInExplorer"), FontWeight = FontWeights.SemiBold, FontSize = 11.5
        };
        footerText.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        footerPanel.Children.Add(openFolderIcon);
        footerPanel.Children.Add(footerText);

        var open = DockFlyout.FlatButton(footerPanel, (_, _) =>
        {
            _stackFlyout.Close();
            OpenPath(stack.Path);
        });
        open.Margin = new Thickness(0, 10, 0, 0);
        open.Padding = new Thickness(8, 7, 8, 7);
        open.HorizontalAlignment = HorizontalAlignment.Stretch;
        open.HorizontalContentAlignment = HorizontalAlignment.Center;
        root.Children.Add(open);
        return root;
    }

    private void OpenPath(string path)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(path) { UseShellExecute = true });
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException or FileNotFoundException)
        {
            AppLog.Warning($"Cannot open {path}: {ex.Message}");
            ReportError("ErrorOpenFileOrFolder");
        }
    }

    private void StackOpenFolder_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { DataContext: DockStack stack }) OpenPath(stack.Path);
    }

    private void StackRemove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { DataContext: DockStack stack }) return;
        var folders = ConfiguredStackFolders()
            .Where(folder => !string.Equals(folder, stack.Path, StringComparison.OrdinalIgnoreCase)).ToList();
        SaveStackFolders(folders);
    }

    private void AddStack_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new Microsoft.Win32.OpenFolderDialog { Title = UiText.Get("AddStack"), Multiselect = false };
        if (dialog.ShowDialog() != true) return;
        var folders = ConfiguredStackFolders().ToList();
        if (folders.Contains(dialog.FolderName, StringComparer.OrdinalIgnoreCase)) return;
        folders.Add(dialog.FolderName);
        SaveStackFolders(folders);
    }

    private void SaveStackFolders(IReadOnlyList<string> folders)
    {
        var error = _preferences.Save(_preferences.Current with { StackFolders = folders });
        if (error is not null) ReportError(error);
    }

    private void StackButton_PreviewMouseLeftButtonDown(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (sender is Button { DataContext: DockStack stack } && _stacks.Contains(stack))
        {
            _dragStackStartPoint = e.GetPosition(this);
            _draggedStack = stack;
        }
        else
        {
            _draggedStack = null;
        }
    }

    private void StackButton_PreviewMouseLeftButtonUp(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        _draggedStack = null;
    }

    private void AnimateStackDrag(Button button, DockStack stack)
    {
        var icon = GetButtonIcon(button);
        var (scale, translate) = icon != null ? GetIconTransforms(icon) : (null, null);
        var animDuration = TimeSpan.FromMilliseconds(130);
        var easeOut = new CubicEase { EasingMode = EasingMode.EaseOut };

        button.Opacity = 0.85;
        if (scale is not null && translate is not null && _preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation)
        {
            scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(1.10, animDuration) { EasingFunction = easeOut });
            scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1.10, animDuration) { EasingFunction = easeOut });
            translate.BeginAnimation(IsVertical ? TranslateTransform.XProperty : TranslateTransform.YProperty,
                new DoubleAnimation(-4 * BounceDirection, animDuration) { EasingFunction = easeOut });
        }

        try
        {
            DragDrop.DoDragDrop(button, stack, DragDropEffects.Move);
        }
        finally
        {
            button.Opacity = 1.0;
            CancelLiveDrag();
            ResetDisplacements();
            if (scale is not null && translate is not null)
            {
                scale.BeginAnimation(ScaleTransform.ScaleXProperty, new DoubleAnimation(1.0, animDuration) { EasingFunction = easeOut });
                scale.BeginAnimation(ScaleTransform.ScaleYProperty, new DoubleAnimation(1.0, animDuration) { EasingFunction = easeOut });
                translate.BeginAnimation(IsVertical ? TranslateTransform.XProperty : TranslateTransform.YProperty,
                    new DoubleAnimation(0, animDuration) { EasingFunction = easeOut });
            }
        }
    }

    private void StackButton_PreviewMouseMove(object sender, System.Windows.Input.MouseEventArgs e)
    {
        if (e.LeftButton != System.Windows.Input.MouseButtonState.Pressed || _draggedStack is null || sender is not Button button)
        {
            return;
        }

        var currentPoint = e.GetPosition(this);
        var diff = _dragStackStartPoint - currentPoint;
        if (Math.Abs(diff.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(diff.Y) > SystemParameters.MinimumVerticalDragDistance)
        {
            var stack = _draggedStack;
            _draggedStack = null;
            AnimateStackDrag(button, stack);
        }
    }

    private void StackButton_DragOver(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out _))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;
            return;
        }

        if (e.Data.GetDataPresent(typeof(StartButtonDragData)))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = false;
            return;
        }

        if (e.Data.GetDataPresent(typeof(DockStack))
            && sender is Button targetButton
            && targetButton.DataContext is DockStack targetStack
            && _stacks.Contains(targetStack))
        {
            e.Effects = DragDropEffects.Move;
            e.Handled = true;

            if (_preferences.Current.AnimationsEnabled && SystemParameters.ClientAreaAnimation)
            {
                UpdateDragDisplacements(targetButton, e);
            }
        }
        else
        {
            e.Effects = DragDropEffects.None;
            e.Handled = true;
        }
    }

    private void StackButton_DragLeave(object sender, DragEventArgs e)
    {
        if (sender is Button button && GetButtonIcon(button) is { } icon)
        {
            ResetIconDisplacement(icon);
        }
    }

    private void StackButton_Drop(object sender, DragEventArgs e)
    {
        if (TryGetDockItemToken(e, out var droppedToken))
        {
            DropDockItem(droppedToken, e);
            e.Handled = true;
            return;
        }

        if (e.Data.GetData(typeof(DockStack)) is DockStack sourceStack
            && sender is Button targetButton
            && targetButton.DataContext is DockStack targetStack
            && _stacks.Contains(sourceStack)
            && _stacks.Contains(targetStack)
            && !ReferenceEquals(sourceStack, targetStack))
        {
            var oldIndex = _stacks.IndexOf(sourceStack);
            var newIndex = _stacks.IndexOf(targetStack);
            if (oldIndex >= 0 && newIndex >= 0 && oldIndex != newIndex)
            {
                ResetDisplacements();
                _stacks.Move(oldIndex, newIndex);
                SaveStackFolders(_stacks.Select(s => s.Path).ToList());
                AnimateDropSettle(targetButton);
            }
        }
        ResetDisplacements();
        e.Handled = true;
    }

    /// <summary>Pins apps from a settings backup that are not pinned yet and exist on this PC; returns how many were added.</summary>
    internal int ImportPins(IReadOnlyList<PinBackup> pins)
    {
        var added = 0;
        foreach (var pin in pins)
        {
            if (!File.Exists(pin.ExecutablePath)
                || _pinnedApps.Any(app => string.Equals(app.ExecutablePath, pin.ExecutablePath, StringComparison.OrdinalIgnoreCase)))
            {
                continue;
            }

            var (shortcutPath, error) = WindowsTaskbarPinImporter.AddPin(pin.Name, pin.ExecutablePath, pin.Arguments);
            if (error is not null || shortcutPath is null)
            {
                AppLog.Warning($"Import pin {pin.ExecutablePath}: {error}");
                continue;
            }

            _pinnedApps.Add(new DockApp(pin.Name, pin.ExecutablePath)
            {
                Icon = PinnedAppIcon.Load(pin.ExecutablePath, iconLocation: null),
                LaunchArguments = pin.Arguments,
                ShortcutPath = shortcutPath
            });
            added++;
        }

        if (added > 0)
        {
            if (_windowTracker is not null) UpdateRunningApps(VisibleSnapshot);
            ResizeDockToContent();
            _replacementSession?.RefreshPosition();
        }

        return added;
    }

    private void DisposeExtras()
    {
        _recycleBinTimer?.Stop();
        _stackFlyout.Close();
        foreach (var stack in _stacks) stack.Dispose();
        _stacks.Clear();
    }
}
