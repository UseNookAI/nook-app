/**
 * AudioPlayer.kt: plays one track at a time in the app, so a run can be heard without opening a
 * file. A second track stops the first. `TrackPlayer` is the row a finished run shows: a round
 * Play/Pause, the track's bar (click or drag to move in it) and the time.
 */
import { useEffect, useState, useSyncExternalStore, type PointerEvent } from "react";
import { Icon } from "../../components/Icon";
import { clock } from "./format";

interface PlayerState {
  /** The run whose track is loaded, or null. */
  id: string | null;
  playing: boolean;
  time: number;
  duration: number;
}

const IDLE: PlayerState = { id: null, playing: false, time: 0, duration: 0 };

export const player = (() => {
  let audio: HTMLAudioElement | null = null;
  let state: PlayerState = IDLE;
  const listeners = new Set<() => void>();
  const set = (patch: Partial<PlayerState>) => {
    state = { ...state, ...patch };
    listeners.forEach((l) => l());
  };
  const release = () => {
    if (!audio) return;
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
    audio = null;
  };
  return {
    get: () => state,
    subscribe(l: () => void) {
      listeners.add(l);
      return () => {
        listeners.delete(l);
      };
    },
    /** Plays `src` as `id` from where it was, or pauses it when it is the one playing. */
    toggle(id: string, src: string, onError: (message: string) => void) {
      if (state.id === id && audio) {
        if (audio.paused) {
          if (audio.ended || state.time >= state.duration - 0.05) audio.currentTime = 0;
          audio.play().catch((e) => onError(`Could not play the track: ${e instanceof Error ? e.message : String(e)}`));
        } else audio.pause();
        return;
      }
      this.play(id, src, onError);
    },
    /** Plays `src` as `id` from the start, stopping whatever played. */
    play(id: string, src: string, onError: (message: string) => void) {
      release();
      const a = new Audio(src);
      audio = a;
      set({ id, playing: false, time: 0, duration: 0 });
      a.onloadedmetadata = () => audio === a && set({ duration: Number.isFinite(a.duration) ? a.duration : 0 });
      a.ontimeupdate = () => audio === a && set({ time: a.currentTime });
      a.onplay = () => audio === a && set({ playing: true });
      a.onpause = () => audio === a && set({ playing: false });
      a.onended = () => audio === a && set({ playing: false, time: a.duration || state.time });
      a.onerror = () => {
        if (audio !== a) return;
        onError("Could not play the track.");
        release();
        set(IDLE);
      };
      a.play().catch(() => undefined);
    },
    seek(id: string, fraction: number) {
      if (state.id !== id || !audio || !state.duration) return;
      audio.currentTime = Math.max(0, Math.min(1, fraction)) * state.duration;
      set({ time: audio.currentTime });
    },
    /** Stops the track of `id` (all of them when null), as leaving the page or deleting a run does. */
    stop(id: string | null = null) {
      if (id != null && state.id !== id) return;
      release();
      set(IDLE);
    },
  };
})();

export function usePlayer(): PlayerState {
  return useSyncExternalStore(player.subscribe, player.get);
}

export function TrackPlayer({
  id,
  src,
  seconds,
  label,
  onError,
}: {
  id: string;
  src: string;
  /** The track's length when known before it loads. */
  seconds: number;
  /** Beside the time: "German". */
  label?: string;
  onError: (message: string) => void;
}) {
  const state = usePlayer();
  const mine = state.id === id;
  // The track's own length, read from its header before it is played.
  const [length, setLength] = useState(0);
  useEffect(() => {
    const probe = new Audio();
    probe.preload = "metadata";
    probe.onloadedmetadata = () => Number.isFinite(probe.duration) && setLength(probe.duration);
    probe.src = src;
    return () => {
      probe.onloadedmetadata = null;
      probe.removeAttribute("src");
      probe.load();
    };
  }, [src]);
  const duration = mine && state.duration > 0 ? state.duration : length > 0 ? length : seconds;
  const time = mine ? state.time : 0;
  const fraction = duration > 0 ? Math.min(1, time / duration) : 0;
  const playing = mine && state.playing;

  const seekTo = (e: PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    const f = (e.clientX - r.left) / r.width;
    if (!mine) {
      player.play(id, src, onError);
      // Once it has loaded, jump to where the bar was clicked.
      const once = () => {
        if (player.get().duration > 0) player.seek(id, f);
        else window.setTimeout(once, 50);
      };
      window.setTimeout(once, 50);
      return;
    }
    player.seek(id, f);
  };

  return (
    <div className="fl-track">
      <button
        type="button"
        className="fl-track__play"
        title={playing ? "Pause" : "Play"}
        aria-label={playing ? "Pause" : "Play"}
        onClick={() => player.toggle(id, src, onError)}
      >
        <Icon name={playing ? "pause-filled" : "play-filled"} size={16} />
      </button>
      <div
        className="fl-track__bar"
        role="slider"
        aria-label="Position"
        aria-valuemin={0}
        aria-valuemax={Math.round(duration)}
        aria-valuenow={Math.round(time)}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          seekTo(e);
        }}
        onPointerMove={(e) => {
          if (e.buttons === 1 && mine) seekTo(e);
        }}
      >
        <div className="fl-track__fill" style={{ width: `${fraction * 100}%` }} />
        <div className="fl-track__knob" style={{ left: `${fraction * 100}%` }} />
      </div>
      <span className="numeric text-tertiary fl-track__time">
        {clock(time * 1000)} / {clock(duration * 1000)}
      </span>
      {label && <span className="caption text-tertiary fl-track__label">{label}</span>}
    </div>
  );
}
