param([string]$Label = 'state')
# Prints the native taskbar state: visibility of Shell_TrayWnd (Explorer's), ABM_GETSTATE bits, primary work-area
# bottom, OpenTaskBar-Rust journal presence and running OpenTaskBar processes. ASCII only.
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class ShellState {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct APPBARDATA { public int cbSize; public IntPtr hWnd; public uint cb; public uint edge; public RECT rc; public IntPtr lParam; }
  [DllImport("shell32.dll")] public static extern UIntPtr SHAppBarMessage(uint msg, ref APPBARDATA data);
  [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr p, IntPtr a, string c, string n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool SystemParametersInfo(uint a, uint b, ref RECT r, uint c);
  public static uint State() { var d = new APPBARDATA(); d.cbSize = Marshal.SizeOf(d); return (uint)SHAppBarMessage(4, ref d); }
  public static string Taskbar() {
    IntPtr h = IntPtr.Zero; var s = "";
    while ((h = FindWindowEx(IntPtr.Zero, h, "Shell_TrayWnd", null)) != IntPtr.Zero) {
      uint pid; GetWindowThreadProcessId(h, out pid);
      var name = ""; try { name = System.Diagnostics.Process.GetProcessById((int)pid).ProcessName; } catch {}
      s += name + ":" + (IsWindowVisible(h) ? "visible" : "hidden") + " ";
    }
    return s.Trim();
  }
  public static int WorkBottom() { var r = new RECT(); SystemParametersInfo(0x30, 0, ref r, 0); return r.B; }
}
'@
$journal = Test-Path (Join-Path $env:LOCALAPPDATA 'OpenTaskBar-Rust\taskbar-state.txt')
$procs = (Get-Process OpenTaskBar, OpenTaskbar, OpenTaskbarWatchdog -ErrorAction SilentlyContinue | ForEach-Object { "$($_.ProcessName)#$($_.Id)" }) -join ','
"[{0}] taskbar={1} state={2} workBottom={3} rustJournal={4} procs={5}" -f $Label, [ShellState]::Taskbar(), [ShellState]::State(), [ShellState]::WorkBottom(), $journal, $procs
