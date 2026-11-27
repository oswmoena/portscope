# PortScope

Panel de escritorio (Tauri 2 + Rust) para ver **qué app está usando cada puerto** en tu máquina, inspirado en el Control Panel de XAMPP.

- Lista los procesos escuchando en TCP con su **nombre de proyecto real** (lee `package.json`, `Cargo.toml`, `pyproject.toml`…), no solo `node` o `python`.
- Detecta el framework (Next.js, Vite, Django, Postgres…) y los **contenedores Docker** con puertos publicados.
- Botón **Puerto libre**: sugiere el siguiente puerto disponible y lo copia al portapapeles.
- Acciones por fila: abrir en el navegador, abrir carpeta, abrir en VS Code, detener (`SIGTERM` / `docker stop`).
- Log de actividad: qué app abrió o liberó qué puerto, en tiempo real.
- **Barra de menú (macOS) / bandeja (Windows):** ve tus apps y sus puertos sin abrir la ventana; cerrar la ventana deja PortScope corriendo ahí.

## Descargar

Instaladores para **macOS** (universal) y **Windows** en [Releases](../../releases/latest).

Los instaladores no están firmados con un certificado de desarrollador:
- **macOS:** la primera vez, clic derecho en la app → *Abrir* (o *Ajustes del Sistema → Privacidad y seguridad → Abrir igualmente*).
- **Windows:** si aparece SmartScreen, *Más información → Ejecutar de todas formas*.

## Desarrollo

Requisitos: Node 18+, Rust (rustup), Xcode Command Line Tools.

```bash
npm install
npm run tauri dev      # app en modo desarrollo
npm run tauri build    # genera .app / .dmg en src-tauri/target/release/bundle
cd src-tauri && cargo test   # tests de los parsers
```

## Cómo funciona

| Dato | macOS / Linux | Windows |
|---|---|---|
| PID y puertos | `lsof -iTCP -sTCP:LISTEN` | `netstat -ano` |
| Carpeta del proceso | `lsof -d cwd` | crate `sysinfo` |
| Comando completo | `ps -o command=` | crate `sysinfo` |
| Contenedores | `docker ps` | `docker ps` |

El backend vive en `src-tauri/src/ports.rs` (escaneo y detección) y `actions.rs` (acciones).

## Publicar una versión

```bash
git tag v0.1.1 && git push origin v0.1.1
```

GitHub Actions compila los instaladores de macOS y Windows y los publica en Releases.
