/** Folders of earlier sessions (knownRepositories), read again when the sessions change. */
import { useEffect, useState } from "react";
import { codeRecentRepositories, onCodeChanged } from "../../api/code";

export function useRepositories(): string[] {
  const [repositories, setRepositories] = useState<string[]>([]);
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const read = () =>
      codeRecentRepositories()
        .then((r) => alive && setRepositories(r))
        .catch(() => undefined);
    read();
    const off = onCodeChanged(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(read, 1000);
    });
    return () => {
      alive = false;
      window.clearTimeout(timer);
      off();
    };
  }, []);
  return repositories;
}
