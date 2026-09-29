/**
 * The window's title strip (NookTopBar.kt). It sits on the canvas colour together with the sidebar,
 * so the only things drawn are the brand mark, the sidebar toggle, the pages (Chat, Code, Nooklets:
 * `nav`), and at the right the update badge, the light | dark switch and the window controls. The whole strip drags the window
 * (`data-tauri-drag-region`; a double-click maximises); its passive parts let clicks through to it.
 *
 * `SetupHeader` is the transparent strip of the welcome and erase screens: window controls only.
 *
 * On a Mac the controls are macOS's traffic lights, over the strip's left end, so the strip starts
 * after them (`nk-topbar--mac`) and still drags the window.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button } from "../components/Button";
import { Icon } from "../components/Icon";
import { FanMark } from "./FanMark";
import { isMac } from "./platform";
import { WindowControls } from "./WindowControls";
import "./shell.css";

export interface TopBarProps {
  isSidebarVisible: boolean;
  isSidebarCollapsed: boolean;
  onToggleSidebar: () => void;
  showUpdateBadge: boolean;
  onUpdateClick: () => void;
  onClose: () => void;
  isDarkTheme: boolean;
  onToggleTheme?: () => void;
  /** The pages to switch between, after the sidebar toggle. */
  nav?: ReactNode;
}

export function TopBar({
  isSidebarVisible,
  isSidebarCollapsed,
  onToggleSidebar,
  showUpdateBadge,
  onUpdateClick,
  onClose,
  isDarkTheme,
  onToggleTheme,
  nav,
}: TopBarProps) {
  return (
    <div className={isMac ? "nk-topbar nk-topbar--mac" : "nk-topbar"} data-tauri-drag-region>
      {isSidebarVisible && (
        <>
          <BrandMark showWordmark={!isSidebarCollapsed} />
          <SidebarToggle collapsed={isSidebarCollapsed} onClick={onToggleSidebar} />
        </>
      )}
      {nav}
      <div className="nk-topbar__spacer" />
      {showUpdateBadge && <Button text="Update available" className="nk-topbar__update" onClick={onUpdateClick} />}
      {onToggleTheme && <ThemeToggle isDark={isDarkTheme} onToggle={onToggleTheme} />}
      <WindowControls onClose={onClose} />
    </div>
  );
}

/** `onScrim`: over the erase screen's dark backdrop, where the controls are drawn light. */
export function SetupHeader({ onClose, onScrim = false }: { onClose: () => void; onScrim?: boolean }) {
  return (
    <div className={onScrim ? "nk-topbar nk-topbar--setup nk-topbar--on-scrim" : "nk-topbar nk-topbar--setup"} data-tauri-drag-region>
      <WindowControls onClose={onClose} />
    </div>
  );
}

/** The mark, which spins while Nook works the GPU, with the "Nook" wordmark beside it. */
export function BrandMark({ showWordmark }: { showWordmark: boolean }) {
  return (
    <div className="nk-brand">
      <FanMark size={24} />
      {showWordmark && <span className="nk-brand__word">Nook</span>}
    </div>
  );
}

/**
 * The sidebar toggle, drawn like icons/layout-left.svg: a window with the sidebar's edge as its
 * middle line. While the sidebar is collapsed the line slides to the window's left edge, as the
 * sidebar has, the way Claude's does.
 */
export function SidebarToggle({ collapsed, onClick }: { collapsed: boolean; onClick: () => void }) {
  const label = collapsed ? "Show sidebar" : "Hide sidebar";
  return (
    <button type="button" className="nk-topbar-button" aria-label={label} title={label} onClick={onClick}>
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
        <rect x="2.5" y="3.5" width="19" height="17" rx="3.5" />
        <line x1="9" y1="3.5" x2="9" y2="20.5" className={collapsed ? "nk-sidebar-edge nk-sidebar-edge--collapsed" : "nk-sidebar-edge"} />
      </svg>
    </button>
  );
}

/**
 * The sun in dark mode and the moon in light, the one a click switches to. On a switch the new one
 * turns and grows in as the old one turns away.
 */
function ThemeToggle({ isDark, onToggle }: { isDark: boolean; onToggle: () => void }) {
  const [leaving, setLeaving] = useState<boolean | null>(null);
  const [animate, setAnimate] = useState(false);
  const previous = useRef(isDark);
  useEffect(() => {
    if (previous.current === isDark) return;
    setLeaving(previous.current);
    setAnimate(true);
    previous.current = isDark;
    const t = window.setTimeout(() => setLeaving(null), 300);
    return () => window.clearTimeout(t);
  }, [isDark]);
  const label = isDark ? "Switch to light mode" : "Switch to dark mode";
  return (
    <button type="button" className="nk-topbar-button nk-theme-toggle" aria-label={label} title={label} onClick={onToggle}>
      {leaving !== null && (
        <span key={`out-${leaving}`} className="nk-theme-toggle__icon nk-theme-toggle__icon--out">
          <Icon name={leaving ? "sun" : "moon"} size={16} />
        </span>
      )}
      <span key={`in-${isDark}`} className={animate ? "nk-theme-toggle__icon nk-theme-toggle__icon--in" : "nk-theme-toggle__icon"}>
        <Icon name={isDark ? "sun" : "moon"} size={16} />
      </span>
    </button>
  );
}
