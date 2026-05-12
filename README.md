<div align="center">
  <a href="https://sarp.es/" target="_blank">
    <img style="width: 200px;" src="https://i.imgur.com/EFK15nk.png" alt="San Andreas Roleplay"/>
  </a>
  <h1>San Andreas Roleplay - Launcher</h1>
  <p>Launcher optimizado para la comunidad de SA:MP de San Andreas Roleplay.</p>
</div>

![Discord Shield](https://discord.com/api/guilds/1118327104908570654/widget.png?style=shield)
[![License: MIT](https://img.shields.io/badge/License-MIT%20License-yellow.svg)](https://opensource.org/licenses/MIT)

## Descripción

Este es un proyecto de launcher moderno desarrollado en **Electron + Vue 3** para mejorar la experiencia de conexión y gestión de mods en el servidor de [San Andreas Roleplay](https://sarp.es). 

> [!NOTE]
> Este repositorio contiene una versión mejorada y en desarrollo constante. No es el código base del launcher original, sino una contribución activa para modernizar sus funciones y estética.

## Características

- 🏎️ **Mod Manager:** Catálogo de mods con instalación automática de CLEO, plugins ASI y ModLoader.
- 🔥 **Anticheat Integrado:** Escaneo de integridad para asegurar un juego limpio.
- 📡 **Server Status:** Información en tiempo real del estado del servidor.
- 🔄 **Auto-Updates:** Sistema de actualización automática integrado mediante GitHub Releases.
- 💎 **UI Premium:** Interfaz oscura, moderna y optimizada.

## Para Desarrolladores

Si deseas contribuir al desarrollo o probarlo localmente:

```bash
# Instalar dependencias
yarn install

# Ejecutar en modo desarrollo (Vite + Electron)
yarn dev

# Formatear y verificar calidad de código
yarn polish
```

## Sistema de Actualización

Este repositorio está configurado para distribuir actualizaciones mediante **GitHub Releases** usando `electron-updater`. 

> [!IMPORTANT]
> Es importante notar que en la **versión original/oficial** del launcher, las actualizaciones no se distribuyen por GitHub, sino a través de un **CDN (BunnyCDN)**. Esta implementación en GitHub es específica para esta rama de desarrollo y contribución.

### Pasos para publicar una versión en este repositorio:
1. Sube la versión en `package.json` (ej: `"1.0.1"`).
2. Asegúrate de tener tu un `GH_TOKEN` válido en el `.env`.
3. Ejecuta el comando de publicación:
   ```bash
   yarn electron-builder --publish always
   ```
4. Los archivos (`.exe`, `.blockmap`, `latest.yml`) se subirán automáticamente a los Releases de este repositorio.

## Licencia

Este proyecto está bajo la licencia MIT — ver el archivo [LICENSE](LICENSE) para más detalles.

---
*Contribución para la comunidad de San Andreas Roleplay.*
