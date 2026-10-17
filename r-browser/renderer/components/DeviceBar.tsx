import type { DevicePreset, EmulationState } from '@shared/contract'
import { Seg } from './Bits'
import { IconRotate } from './Icons'

const ZOOMS = [
  { value: '0', label: 'fit' },
  { value: '0.5', label: '50%' },
  { value: '0.75', label: '75%' },
  { value: '1', label: '100%' },
] as const

/**
 * The emulated environment, edited as one unit.
 *
 * Every control here writes a whole `EmulationState` through one call, because
 * that is what `rbrowse.emulation.set` takes — and because the alternative
 * (a call per field) would let rotation and the device preset arrive in either
 * order, which is a landscape phone that is briefly a portrait desktop.
 */
export function DeviceBar({
  devices, state, slot, onChange,
}: {
  devices: DevicePreset[]
  state: EmulationState
  /** The page slot's own size, which is what "fit the window" resolves to. It
   *  is the honest readout in responsive mode: there is no preset to quote. */
  slot: { width: number; height: number }
  onChange: (next: EmulationState) => void
}) {
  const device = devices.find(d => d.id === state.deviceId) ?? null
  const set = (patch: Partial<EmulationState>) => onChange({ ...state, ...patch })

  // Grouped exactly as the preset says, and in the order the groups first
  // appear — main owns that ordering and re-sorting it here would be a second
  // opinion about which device is most reached for.
  const groups: Array<[string, DevicePreset[]]> = []
  for (const d of devices) {
    const found = groups.find(g => g[0] === d.group)
    if (found) found[1].push(d)
    else groups.push([d.group, [d]])
  }

  const width = device
    ? (state.rotated ? device.height : device.width)
    : (state.customWidth ?? slot.width)
  const height = device
    ? (state.rotated ? device.width : device.height)
    : (state.customHeight ?? slot.height)

  return (
    <div className="devicebar">
      <Seg
        value={state.deviceId ? 'device' : 'responsive'}
        options={[
          { value: 'responsive', label: 'responsive' },
          { value: 'device', label: 'device' },
        ]}
        onChange={v => {
          if (v === 'responsive') set({ deviceId: null })
          // Switching *into* device mode with nothing chosen would be a mode
          // with no effect, so it lands on the first preset.
          else set({ deviceId: state.deviceId ?? devices[0]?.id ?? null })
        }}
      />

      {state.deviceId ? (
        <div className="group">
          <select
            className="field"
            style={{ width: 176 }}
            value={state.deviceId}
            onChange={e => set({ deviceId: e.target.value })}
          >
            {groups.map(([group, list]) => (
              <optgroup key={group} label={group}>
                {list.map(d => <option key={d.id} value={d.id}>{d.label}</option>)}
              </optgroup>
            ))}
          </select>
          <button
            type="button"
            className="chip"
            aria-pressed={state.rotated}
            title="rotate"
            onClick={() => set({ rotated: !state.rotated })}
          >
            <IconRotate size={11} />
          </button>
          <Seg
            value={String(state.zoom)}
            options={ZOOMS}
            onChange={v => set({ zoom: Number(v) })}
            title="scale of the device frame"
          />
        </div>
      ) : (
        <div className="group">
          {/* Free size. Empty means "fit the window", which is why these are
              not defaulted to the slot's pixels: a number typed into the box is
              a *choice*, and pre-filling it would make every window resize look
              like the operator had pinned a width. */}
          <span className="micro">size</span>
          <input
            className="field field-num data"
            inputMode="numeric"
            placeholder="fit"
            value={state.customWidth ?? ''}
            onChange={e => set({ customWidth: numberOrUndefined(e.target.value) })}
          />
          <span className="micro faint">×</span>
          <input
            className="field field-num data"
            inputMode="numeric"
            placeholder="fit"
            value={state.customHeight ?? ''}
            onChange={e => set({ customHeight: numberOrUndefined(e.target.value) })}
          />
        </div>
      )}

      <div className="group">
        <span className="micro">scheme</span>
        <Seg
          value={state.colorScheme}
          options={[
            { value: 'light', label: 'light' },
            { value: 'dark', label: 'dark' },
            { value: 'auto', label: 'auto' },
          ]}
          onChange={v => set({ colorScheme: v })}
          title="prefers-color-scheme"
        />
      </div>

      <label className="check">
        <input
          type="checkbox"
          checked={state.reducedMotion}
          onChange={e => set({ reducedMotion: e.target.checked })}
        />
        reduced motion
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={state.forcedColors}
          onChange={e => set({ forcedColors: e.target.checked })}
        />
        forced colors
      </label>

      {/* The readout. A screenshot taken at 3× reads as a desktop layout unless
          the ratio travels with it, so the ratio is on screen too. */}
      <span className="micro data" style={{ marginLeft: 'auto' }}>
        {Math.round(width)} × {Math.round(height)}
        {device ? ` @${device.deviceScaleFactor}` : ''}
        {device?.mobile ? ' · mobile' : ''}
        {device?.touch ? ' · touch' : ''}
      </span>
    </div>
  )
}

/** A cleared box is "fit", not zero — and a typed non-number must not become
 *  `NaN`, which would travel into CDP as a viewport of no width. */
function numberOrUndefined(raw: string): number | undefined {
  const n = Number.parseInt(raw.replace(/\D+/g, ''), 10)
  return Number.isFinite(n) && n > 0 ? n : undefined
}
