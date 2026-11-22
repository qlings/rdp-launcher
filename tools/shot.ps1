# 截窗口图：PrintWindow 拿窗口内容，不依赖桌面是否可见（RDP 断开也能截）
param(
    [string]$ProcessName = "rdp-launcher",
    [string]$OutFile = "C:\rdp\shot.png"
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing.Common
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;
public class WinShot {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left,Top,Right,Bottom; }
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
}
'@
$p = Get-Process $ProcessName -ErrorAction Stop | Select-Object -First 1
$p.Refresh()
$hwnd = $p.MainWindowHandle
if ($hwnd -eq [IntPtr]::Zero) { throw "窗口句柄还没出来" }
$r = New-Object WinShot+RECT
[void][WinShot]::GetWindowRect($hwnd, [ref]$r)
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
$bmp = [System.Drawing.Bitmap]::new($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
$ok = [WinShot]::PrintWindow($hwnd, $hdc, 2)
$g.ReleaseHdc($hdc)
$bmp.Save($OutFile, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Host "saved $OutFile (${w}x${h}) PrintWindow=$ok"
