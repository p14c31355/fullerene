# Solvent Runtime API

Solvent is the no_std orchestration layer between kernel integration and the
Lattice, Nozzle, Resonance, and ChronoLine subsystems. Callers continue to use
the `solvent` crate root; internal modules define responsibility boundaries
without changing the public entry points.

## Runtime lifecycle

| API | Responsibility |
|---|---|
| `init()` | Initialize desktop state, timers, event queue, and dispatcher |
| `is_initialized()` | Report whether runtime state is installed |
| `tick_core(now)` | Poll input, advance timers and services, and dispatch events |
| `runtime_tick(now, framebuffer)` | Run a framebuffer-backed update and render |
| `runtime_tick_no_fb()` | Run an update using the registered render callback |

## Module boundaries

| Internal module | Responsibility |
|---|---|
| `runtime_context` | Runtime state definitions, configuration, and initialization |
| `input_loop` | Mouse/keyboard polling and input translation |
| `event_loop` | Timers, services, event dispatch, and frame pacing |
| `window_api` | Window lifecycle, redraw control, and file launching |
| `callbacks` | Kernel callbacks and boundary transfer types |
| `services` | Service registration, action queues, and UI snapshots |

The modules are private implementation details. Public functions, types, and
statics are re-exported from the crate root for compatibility.

## Ownership note

This split does not change singleton ownership. Moving `SOLVENT_CALLBACKS`,
runtime state, the event queue, and the dispatcher into an explicit owned
`RuntimeContext` is a separate lifecycle change.
