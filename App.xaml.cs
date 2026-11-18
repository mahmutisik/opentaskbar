using System.Diagnostics;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Threading;
using Microsoft.Win32;
using OpenTaskbar.Services;

namespace OpenTaskbar;

/// <summary>
/// Interaction logic for App.xaml
/// </summary>
public partial class App : Application
{
    private Mutex? _instanceMutex;
    private int _crashHandled;
    public ApplicationPreferences Preferences { get; private set; } = new(new DockSettingsStore());
    public bool IsPreview { get; private set; }
    public bool IsInspection { get; private set; }
    public bool IsDev { get; private set; }
    public bool OpenSettingsOnStartup { get; private set; }
    private DevReloadService? _devReload;

    protected override void OnStartup(StartupEventArgs e)
    {
        if (TaskbarRecoveryWatchdog.IsWatchdogInvocation(e.Args))
        {
            TaskbarRecoveryWatchdog.Run(e.Args);
            Environment.Exit(0);
            return;
        }

        if (e.Args.Contains("--restore", StringComparer.OrdinalIgnoreCase))
        {
            var restoreError = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            if (restoreError is not null) Trace.TraceError(restoreError);
            // A crashed tray host leaves icons registered with its destroyed window; ask apps to re-add them to Explorer.
            OpenTaskbar.Services.TrayHost.BroadcastTaskbarCreated();
            Shutdown(restoreError is null ? 0 : 1);
            return;
        }

        if (e.Args.Contains("--software-render", StringComparer.OrdinalIgnoreCase))
        {
            System.Windows.Media.RenderOptions.ProcessRenderMode = System.Windows.Interop.RenderMode.SoftwareOnly;
        }

        AppLog.Initialize();
        // A previous instance may have been killed while the taskbar was hidden. Heal before choosing a mode:
        // --preview and --inspect never start a replacement session, so they would otherwise leave it hidden.
        var healError = TaskbarReplacementSession.HealPendingRecovery();
        if (healError is not null) AppLog.Error($"Startup taskbar self-heal failed: {healError}");
        DispatcherUnhandledException += App_DispatcherUnhandledException;
        AppDomain.CurrentDomain.UnhandledException += CurrentDomain_UnhandledException;
        TaskScheduler.UnobservedTaskException += TaskScheduler_UnobservedTaskException;
        SystemEvents.SessionSwitch += SystemEvents_SessionSwitch;

        IsPreview = e.Args.Contains("--preview", StringComparer.Ordinal);
        IsInspection = e.Args.Contains("--inspect", StringComparer.Ordinal);
        OpenSettingsOnStartup = e.Args.Contains("--settings", StringComparer.Ordinal);
        IsDev = e.Args.Contains("--dev", StringComparer.Ordinal);
        var sourceArg = e.Args.FirstOrDefault(arg => arg.StartsWith("--source=", StringComparison.OrdinalIgnoreCase));
        var sourceRoot = sourceArg?["--source=".Length..];
        if (IsPreview)
            Preferences = new ApplicationPreferences(new DockSettingsStore(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "OpenTaskbar", "preview-settings.json")));
        UiText.Current.SetLanguage(Preferences.Current.Language);
        ThemeManager.Apply(Preferences.Current.Theme, Preferences.Current.SurfaceOpacity);
        _instanceMutex = new Mutex(initiallyOwned: true,
            name: IsDev ? "Local\\OpenTaskbar.Dev" : IsPreview ? "Local\\OpenTaskbar.Preview" : "Local\\OpenTaskbar.Singleton", out var createdNew);
        if (!createdNew)
        {
            AppLog.ReportError("AlreadyRunning");
            _instanceMutex.Dispose();
            _instanceMutex = null;
            Shutdown();
            return;
        }

        var themeInstallError = ThemePacks.InstallBuiltInThemes();
        // Before any window exists, so a widget pack's sizes and fonts are in place when styles are first built.
        ThemePacks.Select(Preferences.Current.ThemePack);
        ThemePacks.ApplyWidgets(Preferences.Current.Theme);
        base.OnStartup(e);

        var mainWindow = new MainWindow();
        MainWindow = mainWindow;
        mainWindow.Show();
        if (themeInstallError is not null)
            AppLog.Error($"Built-in theme installation: {themeInstallError}");

        if (IsDev)
            StartDevReload(sourceRoot);
    }

    private void SystemEvents_SessionSwitch(object sender, SessionSwitchEventArgs e)
    {
        var unlocking = e.Reason is SessionSwitchReason.SessionUnlock or SessionSwitchReason.SessionLogon;
        var locking = e.Reason is SessionSwitchReason.SessionLock or SessionSwitchReason.SessionLogoff;
        if (!unlocking && !locking) return;

        // SystemEvents raises this on a thread-pool thread; every UI state change must happen on the dispatcher.
        Dispatcher.BeginInvoke(new Action(() =>
            (MainWindow as OpenTaskbar.MainWindow)?.ResetTransientUiStateForSessionSwitch(unlocking)));
    }

    private void StartDevReload(string? sourceRoot)
    {
        var root = sourceRoot;
        if (string.IsNullOrWhiteSpace(root) || !Directory.Exists(root))
        {
            // Default to the project root when launched from bin/<config>/<tfm>.
            root = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "..", "..", ".."));
        }

        if (!Directory.Exists(Path.Combine(root, "Design")))
        {
            AppLog.Warning($"DevReload: source root '{root}' has no Design folder; reload disabled.");
            return;
        }

        _devReload = new DevReloadService(Dispatcher, root);
        AppLog.Info($"DevReload: watching {root}");
    }

    protected override void OnExit(ExitEventArgs e)
    {
        TaskbarRecoveryWatchdog.NotifyCleanExit();
        SystemEvents.SessionSwitch -= SystemEvents_SessionSwitch;
        var error = (MainWindow as OpenTaskbar.MainWindow)?.RestoreWindowsTaskbar();
        if (error is not null)
        {
            AppLog.Error(error);
            e.ApplicationExitCode = 1;
        }

        // The window-level restore can fail while Explorer is restarting. A leftover journal is the reliable
        // signal that the taskbar may still be hidden, so try the journal-based path before shutting down.
        if (TaskbarReplacementSession.HasPendingRecovery)
        {
            var recoveryError = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            if (recoveryError is not null)
            {
                AppLog.Error($"Exit taskbar self-heal failed: {recoveryError}");
                e.ApplicationExitCode = 1;
            }
        }

        _devReload?.Dispose();
        _devReload = null;
        _instanceMutex?.ReleaseMutex();
        _instanceMutex?.Dispose();
        _instanceMutex = null;
        AppLog.Info("OpenTaskbar exited.");
        AppLog.Shutdown();
        base.OnExit(e);
    }

    protected override void OnSessionEnding(SessionEndingCancelEventArgs e)
    {
        TaskbarRecoveryWatchdog.NotifyCleanExit();
        var error = (MainWindow as OpenTaskbar.MainWindow)?.RestoreWindowsTaskbar();
        if (error is not null) AppLog.Error(error);
        if (TaskbarReplacementSession.HasPendingRecovery)
        {
            error = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            if (error is not null) AppLog.Error(error);
        }

        base.OnSessionEnding(e);
    }

    private void App_DispatcherUnhandledException(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        e.Handled = true;
        HandleFatalError(e.Exception, "UI thread");
        Shutdown(1);
    }

    private void CurrentDomain_UnhandledException(object sender, UnhandledExceptionEventArgs e)
    {
        // The process is terminating; restore from this thread because the UI thread may be gone.
        HandleFatalError(e.ExceptionObject as Exception, "background thread");
    }

    private static void TaskScheduler_UnobservedTaskException(object? sender, UnobservedTaskExceptionEventArgs e)
    {
        AppLog.Warning($"Unobserved task exception: {e.Exception}");
        e.SetObserved();
    }

    private void HandleFatalError(Exception? exception, string origin)
    {
        if (Interlocked.Exchange(ref _crashHandled, 1) != 0) return;
        AppLog.Error($"Unhandled exception on {origin}: {exception}");
        var hasSession = TaskbarReplacementSession.HasActiveSession;
        if (!hasSession && !TaskbarReplacementSession.HasPendingRecovery)
        {
            Trace.Flush();
            return;
        }

        var error = hasSession ? TaskbarReplacementSession.EmergencyRestore() : null;
        // EmergencyRestore reports "in progress" when another thread is already stopping. The journal is the
        // authoritative signal, so fall back to the journal-based path whenever one is still left behind.
        if (error is not null || TaskbarReplacementSession.HasPendingRecovery)
        {
            var forced = TaskbarReplacementSession.ForceRestoreNativeTaskbar();
            if (forced is null) error = null;
            else error ??= forced;
        }

        if (error is not null) AppLog.Error($"Emergency taskbar restore failed: {error}");
        else AppLog.Info("Windows taskbar restored after an unhandled exception.");
        Trace.Flush();
    }
}
