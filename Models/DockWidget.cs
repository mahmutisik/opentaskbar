using System.ComponentModel;

namespace OpenTaskbar.Models;

/// <summary>
/// Data model item representing a dock widget tile (e.g. clock, battery, countdown, stopwatch).
/// </summary>
public class DockWidgetItem(string id, string label) : INotifyPropertyChanged
{
    private string _value = string.Empty;
    private string _label = label;

    public string Id { get; } = id;

    public string Label
    {
        get => _label;
        set
        {
            if (_label == value) return;
            _label = value;
            NotifyChanged(nameof(Label));
        }
    }

    public string Value
    {
        get => _value;
        set
        {
            if (_value == value) return;
            _value = value;
            NotifyChanged(nameof(Value));
        }
    }

    private bool _isCharging;

    /// <summary>Battery widget: the device is charging, drawn as a bolt on the battery icon.</summary>
    public bool IsCharging
    {
        get => _isCharging;
        set
        {
            if (_isCharging == value) return;
            _isCharging = value;
            NotifyChanged(nameof(IsCharging));
        }
    }

    private bool _isBatteryLow;

    /// <summary>Battery widget: low and running on battery, drawn as a red card.</summary>
    public bool IsBatteryLow
    {
        get => _isBatteryLow;
        set
        {
            if (_isBatteryLow == value) return;
            _isBatteryLow = value;
            NotifyChanged(nameof(IsBatteryLow));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    protected void NotifyChanged(string propertyName) => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
}

/// <summary>
/// Alias for <see cref="DockWidgetItem"/> to preserve compatibility.
/// </summary>
public class DockWidget(string id, string label) : DockWidgetItem(id, label)
{
}
