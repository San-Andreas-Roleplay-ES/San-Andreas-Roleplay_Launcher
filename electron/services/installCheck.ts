// esto evita que el front llame a samp_install, gta_install o cache_install varias veces al mismo tiempo
// esto evita varias descargas al mismop  tiempo o conflictos de archivos

export type TaskKind = "samp" | "gta" | "cache"
let runningTask: TaskKind | null = null

export function tryLock(task: TaskKind): boolean {
    if (runningTask !== null) return false
    runningTask = task
    return true
}

export function unlock(task: TaskKind): void {
    if (runningTask === task) {
        runningTask = null
    }
} /** devuelve que la tarea este corriendo o null si no hay */

export function currentTask(): TaskKind | null {
    return runningTask
}
