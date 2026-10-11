# Windows graphics drivers

This guide targets **Windows 11 x86_64**, prioritizing **RTX PRO 4000 Blackwell and RTX PRO 2000
Blackwell**, alongside GeForce RTX 30 / 40 / 50 series and Intel integrated graphics. Windows
runtime and physical-GPU testing were not available in this environment; the driver choices
below follow the manufacturers' documentation and PdfCraft's implemented renderer controls.

## Choose the driver for your hardware

| GPU | Driver choice | Official guidance |
| --- | --- | --- |
| RTX PRO 4000 / 2000 Blackwell | Current NVIDIA RTX Enterprise **Production Branch** for your exact desktop, SFF or laptop model and Windows 11 64-bit | [NVIDIA driver search](https://www.nvidia.com/en-us/drivers/); [supported RTX PRO Blackwell example](https://www.nvidia.com/en-us/drivers/details/279889/) |
| GeForce RTX 30 / 40 / 50 | **Studio Driver** for creative applications; Game Ready is also an option when gaming is the priority | [NVIDIA driver search](https://www.nvidia.com/en-us/drivers/); [Studio supported-product example](https://www.nvidia.com/en-us/drivers/details/280101/) |
| Intel integrated graphics | The PC manufacturer's graphics driver first; Intel Driver & Support Assistant can identify the appropriate generic driver when needed | [Intel OEM guidance](https://www.intel.com/content/www/us/en/support/articles/000058295/graphics.html), [driver selection by generation](https://www.intel.com/content/www/us/en/support/articles/000090440/graphics.html) |

Select the exact card and operating system in the driver search and check its supported-product
list. The linked NVIDIA releases demonstrate model support; they are not minimum-version claims
or pinned downloads. Intel's Arc/Core Ultra, older Iris/UHD and legacy graphics families have
different driver branches, so use the hardware detection or generation table.

Install the selected graphics driver using its normal installer and restart Windows when requested.
On a laptop with Intel and NVIDIA graphics, keep **both** drivers installed: the integrated GPU may
drive the built-in display even when an application renders on NVIDIA. PdfCraft uses the graphics
driver's Direct3D/OpenGL support; a CUDA toolkit or Vulkan SDK is not needed.

## Check the active GPU

Run `dxdiag` from Start and inspect its Display and Render tabs for the GPU name and driver
version. [Microsoft Basic Display Adapter](https://support.microsoft.com/en-us/windows/hardware/display-graphics/microsoft-basic-display-adapter-in-windows)
indicates that Windows is using its generic display driver. On NVIDIA systems, `nvidia-smi` in a
terminal can also confirm the installed driver and GPU model.

Start PdfCraft with its normal settings first. On Windows it uses **Direct3D 12**, with OpenGL
available as a fallback. PdfCraft normally chooses the GPU driving the primary display and logs
its selection; when display information is unavailable, its default preference is the integrated
GPU. Logs are under `%APPDATA%\PdfCraft\data\logs\pdfcraft.log`, or `PdfCraftData\logs\` beside
the executable for a portable installation.

For hybrid graphics, Windows also offers a per-app choice under **Settings → System → Display →
Graphics → PdfCraft → Options**. Select the preference, save it and restart the app; see
[Microsoft's instructions](https://support.microsoft.com/en-us/windows/hardware/display-graphics/optimizations-for-windowed-games-in-windows-11).
Check the actual adapter in the log, since PdfCraft's display selection can take precedence over
the operating system's preference.

## Renderer diagnostics in PowerShell

For the MSI installation, the default executable is:

```powershell
$pdfcraft = Join-Path $env:ProgramFiles 'PdfCraft\pdfcraft.exe'
& $pdfcraft
```

For a portable installation, set `$pdfcraft` to the executable you extracted. To request a
high-performance adapter through PdfCraft's own selection path:

```powershell
$env:WGPU_POWER_PREF = 'high'
& $pdfcraft
Remove-Item Env:WGPU_POWER_PREF
```

This requests a suitable high-performance GPU; it does not identify one particular card when
several are installed. If investigating a driver or blank-window problem, try the OpenGL renderer:

```powershell
$env:PDFCRAFT_RENDERER = 'gl'
& $pdfcraft
Remove-Item Env:PDFCRAFT_RENDERER
```

Close PdfCraft between these runs. The environment settings affect launches from that PowerShell
session. Include the GPU/driver versions and PdfCraft log when reporting a problem. More controls
are described in the [development guide](development.md#environment-variables).
