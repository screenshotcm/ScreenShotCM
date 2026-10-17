# Desarrollo de ScreenShotCM

Cómo compilar, probar y empaquetar la aplicación. Para usarla, ver el
[README](README.md).

## Compilar

Rust con el objetivo `x86_64-pc-windows-gnullvm` y el enlazador de
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw) — sin Visual Studio.

1. Instalar [rustup](https://rustup.rs) y fijar el host gnullvm, para que
   `rust-toolchain.toml` (canal `stable`) use `stable-x86_64-pc-windows-gnullvm`:
   `rustup set default-host x86_64-pc-windows-gnullvm`.
2. Descomprimir llvm-mingw (`*-ucrt-x86_64.zip`, la versión que fija
   `.github/workflows/build.yml`) en `%LOCALAPPDATA%\Programs\llvm-mingw`, o
   apuntar `LLVM_MINGW` a donde esté.
3. Compilar y probar a través del script, que pone llvm-mingw en el PATH:

```powershell
./scripts/cargo.ps1 test
./scripts/cargo.ps1 clippy --release --all-targets '--' -D warnings
./scripts/cargo.ps1 build --release
```

## Instalador (MSI)

Necesita [WiX 5.0.2](https://wixtoolset.org)
(`dotnet tool install --global wix --version 5.0.2`; las versiones 6 y 7
exigen una cuota de mantenimiento):

```powershell
./installer/build.ps1
```

Deja `dist/ScreenShotCM-<versión>.msi`, `dist/ScreenShotCM.msi` (el nombre
fijo del enlace «última versión») y su SHA-256. El MSI se instala por usuario
en `%LOCALAPPDATA%\Programs\ScreenShotCM`, registra el arranque con Windows y
el enlace `screenshotcm://`, y lleva `LICENSE.txt` y
`THIRD_PARTY_LICENSES.md`.

## Integración continua

Cada envío a `main` y cada etiqueta `v*` compilan, prueban, pasan clippy y
empaquetan el MSI en GitHub Actions ([build.yml](.github/workflows/build.yml))
con las mismas herramientas, fijadas por versión y comprobadas por su hash; las
acciones están fijadas por commit. El resultado queda como artefacto de la
ejecución. Dependabot vigila las acciones y el crate.

## Microsoft Store (MSIX, opcional)

`msix/build.ps1` genera `dist/ScreenShotCM-<versión>.msix`. Necesita
`makeappx.exe` del paquete
[Microsoft.Windows.SDK.BuildTools](https://www.nuget.org/packages/Microsoft.Windows.SDK.BuildTools)
descomprimido en `%LOCALAPPDATA%\Programs\windows-sdk-buildtools` (o la
ruta en `SDK_BUILDTOOLS`).

```powershell
./msix/build.ps1 -IdentityName "<Package/Identity/Name>" `
                 -Publisher "<Package/Identity/Publisher>" `
                 -PublisherDisplayName "<Nombre del editor>"
```

Los tres valores salen de Partner Center → la aplicación → *Product
management → Product identity*; sin ellos el paquete lleva valores de prueba.
Se sube sin firmar: la Store lo firma. Dentro del paquete, *Iniciar con
Windows* usa la tarea de inicio declarada en `msix/AppxManifest.xml`, que
también declara el enlace `screenshotcm://`. El texto de la ficha está en
[msix/store-listing.md](msix/store-listing.md).

## Captura con desplazamiento

`scroll.rs` lleva la sesión: un marco alrededor del área, la barra (Desplazar
solo, Listo, Cancelar) y la vista previa, las tres excluidas de la captura y,
el marco y la vista previa, transparentes al ratón. El área se lee cada 60 ms
y *Desplazar solo* envía una muesca de rueda cada tres lecturas mientras el
puntero está sobre el área; termina sola tras 20 lecturas sin nada nuevo.

`stitch.rs` une las lecturas. Cada fila se resume en el brillo medio de 32
franjas, sin el borde derecho donde se mueve la barra de desplazamiento, y se
busca el desplazamiento cuyas filas difieren menos; las bandas que no se
mueven arriba y abajo (encabezados fijos) se guardan una sola vez. No se
comparan píxeles exactos: un navegador al 125 % desplaza en fracciones de
píxel y redibuja el suavizado del texto en cada posición (Edge perdía 20 de 30
lecturas así). La altura se limita a lo que el editor puede dibujar y a lo que
un archivo guardado puede volver a abrirse (20 000 px y 100 megapíxeles).

## Herramientas de mantenimiento

- `screenshotcm.exe --export-icon assets/screenshotcm.ico` regenera el icono
  desde el dibujo de `src/icon.rs`; hágalo tras cambiar el dibujo.
- `screenshotcm.exe --export-store-assets <carpeta>` escribe los mosaicos del
  paquete MSIX; lo llama `msix/build.ps1`.
- Con `SCM_SHOW_IN_CAPTURE=1`, una compilación de depuración deja que sus
  propias ventanas (marcos, barras, vistas previas) salgan en las capturas,
  para poder fotografiarlas en las pruebas. La versión publicada lo ignora.
- Con `SCM_THEME=light` o `SCM_THEME=dark`, una compilación de depuración usa
  esa paleta sin tocar los ajustes ni Windows, para fotografiar las dos.
- Con `SCM_STARTUP_GRACE_SECS=N`, una compilación de depuración acorta los
  120 s durante los que un segundo arranque sin argumentos se ignora (el
  inicio de sesión lanzando la app dos veces), para probar ese caso.
- `cargo test icon_sheet -- --ignored` dibuja todos los iconos en claro y
  oscuro, a tamaño real y ampliados, en `SCM_ICON_SHEET` (o en `%TEMP%`).

## Idiomas

Cada texto de la interfaz está escrito en el código como
`tr("español", "English")`. El idioma sale de *Ajustes → General → Idioma* o,
en automático, del idioma de Windows.

## Rendimiento y memoria

| | ScreenShotCM 0.4.14 |
|---|---|
| Instalador | 504 KB |
| Programa | un solo `.exe` de 960 KB, sin DLL propias |
| Memoria privada en reposo | 1,8 MB |
| Hilos en reposo | 2–4 |

Medido con el agente instalado y en reposo en la bandeja (`Get-Process`,
2026-10-14).

- Las ventanas dibujan con Direct2D por software (WARP): uno por hardware
  carga en el proceso los controladores de cada GPU (130 MB en un portátil con
  dos) y no se descargan nunca. Cada ventana crea su propia fábrica de
  Direct2D; una compartida retenía 90 MB.
- Un fotograma de la selección tarda unos 15 ms sobre 3840×1080 y aparece unos
  100 ms después de la orden.
- Tras capturar, los hilos de trabajo que Windows abre para dibujar se retiran
  solos en menos de un minuto.
- Después de usarla (editor, capturas fijadas), el agente conserva unos pocos
  MB que Windows guarda para reutilizar: tras abrir y cerrar 45 capturas
  fijadas quedó en unos 8 MB y no siguió creciendo.

## Dependencias

Dos crates de la misma familia: [`windows`](https://crates.io/crates/windows)
y `windows-numerics` (MIT OR Apache-2.0). La bandeja, los atajos, el
portapapeles, la codificación de imágenes (WIC) y el dibujo (Direct2D) se usan
directamente desde Windows. Toda dependencia nueva debe ser gratuita y de
licencia permisiva, y anotarse en
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
