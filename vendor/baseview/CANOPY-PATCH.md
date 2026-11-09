Baseview 0.3.7, MIT OR Apache-2.0 (licenses included).

Local change: use EGL 1.5 eglGetPlatformDisplay(EGL_PLATFORM_X11) instead of
eglGetDisplay for the X11 backend. A desktop EGL_PLATFORM=wayland otherwise
causes Mesa to interpret the Xlib display as a Wayland display and crash.
No process environment changes are made. GLX remains the fallback.
