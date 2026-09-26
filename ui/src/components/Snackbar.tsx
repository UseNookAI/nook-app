/**
 * The snackbar host (Compose SnackbarHost): `useSnackbar().say(message)` shows a message at the
 * bottom of the page for a few seconds. One provider sits at the app root.
 */
import { createContext, useCallback, useContext, useRef, useState, type ReactNode } from "react";
import "./components.css";

interface SnackbarApi {
  say: (message: string, long?: boolean) => void;
}

const Ctx = createContext<SnackbarApi>({ say: (m) => console.info(m) });

export function SnackbarProvider({ children }: { children: ReactNode }) {
  const [message, setMessage] = useState<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const say = useCallback((m: string, long = false) => {
    setMessage(m);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setMessage(null), long ? 10000 : 4000);
  }, []);
  return (
    <Ctx.Provider value={{ say }}>
      {children}
      {message && (
        <div className="nk-snackbar body2" role="status" onClick={() => setMessage(null)}>
          {message}
        </div>
      )}
    </Ctx.Provider>
  );
}

export const useSnackbar = () => useContext(Ctx);
