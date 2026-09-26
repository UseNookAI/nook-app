/**
 * Browser preview only (`npm run dev`, then /?gallery, add &dark for the dark theme): every shared
 * component on one page, to look at and click. Never part of the app.
 */
import { useRef, useState } from "react";
import { Chip, LiveDot, QuietAction, TextLink } from "../components/Activity";
import { Button, IconButton, ToolbarIcon } from "../components/Button";
import { Dropdown } from "../components/Dropdown";
import { NumericStepper } from "../components/NumericStepper";
import { DropdownMenu } from "../components/Popover";
import { SidebarIconButton, SidebarItem } from "../components/SidebarItem";
import { useSnackbar } from "../components/Snackbar";
import { ProgressBar, Spinner } from "../components/Spinner";
import { MenuDivider, StyledMenuItem } from "../components/StyledMenuItem";
import { SearchField, TextField } from "../components/TextField";
import { Toggle } from "../components/Toggle";
import { Tooltip } from "../components/Tooltip";
import { PromptActionIconButton, ToolbarPill } from "../screens/hub/ComposerControls";
import { Greeting } from "../screens/hub/Greeting";
import { KindSwitch, type ChatKind } from "../screens/hub/KindSwitch";
import { PagePane } from "../screens/hub/PagePane";
import { TimeOfDayIcon } from "../screens/hub/TimeOfDayIcon";
import { SettingsAction, SettingsDropdown, SettingsGroup, SettingsItemRow, SettingsRowDivider, SettingsSubNav } from "../screens/settings/components";
import { FanMark } from "./FanMark";
import { useTheme } from "./theme";

const row = { display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap" } as const;
const section = { display: "flex", flexDirection: "column", gap: 12, marginBottom: 28 } as const;

export function DevGallery() {
  const { dark, setMode } = useTheme();
  const { say } = useSnackbar();
  const [on, setOn] = useState(true);
  const [steps, setSteps] = useState(4096);
  const [choice, setChoice] = useState("Light");
  const [text, setText] = useState("");
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<ChatKind>("CHAT");
  const [menu, setMenu] = useState(false);
  const [sub, setSub] = useState(0);
  const pill = useRef<HTMLButtonElement>(null);

  return (
    <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
      <div style={{ width: 264, padding: 8, display: "flex", flexDirection: "column", gap: 2 }}>
        <SidebarItem icon="new-chat" label="Chat" isCollapsed={false} isActive />
        <SidebarItem icon="code" label="Code" isCollapsed={false} />
        <SidebarItem icon="plus" label="New chat" isCollapsed={false} accent />
        <div style={{ ...row, padding: "0 8px" }}>
          <SidebarItem icon="settings" label="Settings" isCollapsed />
          <SidebarIconButton icon="dots-horizontal" title="More" onClick={() => say("More")} />
          <LiveDot />
          <LiveDot thinking />
        </div>
      </div>
      <PagePane>
        <div style={{ overflow: "auto", padding: 24 }}>
          <div style={section}>
            <div className="h6">Buttons</div>
            <div style={row}>
              {(["primary", "accent", "secondary", "ghost", "danger", "soft"] as const).map((v) => (
                <Button key={v} text={v} variant={v} onClick={() => say(v)} />
              ))}
              <Button text="disabled" disabled />
              <Button text="compact" compact icon="plus" iconPosition="start" />
              <IconButton icon="copy" title="Copy" />
              <ToolbarIcon icon="copy" hint="A toolbar icon" onClick={() => say("Copy")} />
              <Button text={dark ? "Light theme" : "Dark theme"} variant="secondary" onClick={() => setMode(dark ? "Light" : "Dark")} />
            </div>
          </div>
          <div style={section}>
            <div className="h6">Controls</div>
            <div style={row}>
              <Toggle checked={on} onChange={setOn} label="Toggle" />
              <Toggle checked={false} onChange={() => {}} disabled label="Disabled" />
              <Dropdown options={["Light", "Dark", "System"]} value={choice} onChange={setChoice} />
              <Tooltip text="A tooltip above">
                <QuietAction text="Hover me" icon="help" onClick={() => {}} />
              </Tooltip>
              <Tooltip text="And one below" placement="bottom">
                <Chip text="Chip below" icon="check" />
              </Tooltip>
              <Chip text="Accent" accent icon="check" />
              <TextLink text="A text link" onClick={() => say("Link")} />
            </div>
            <div style={{ ...row, alignItems: "flex-start" }}>
              <div style={{ width: 220 }}>
                <NumericStepper label="Context" value={steps} onChange={setSteps} min={2048} max={4352} step={128} />
              </div>
              <div style={{ width: 260 }}>
                <TextField value={text} onChange={setText} placeholder="A text field" onEnter={() => say(`Enter: ${text}`)} />
              </div>
              <SearchField value={query} onChange={setQuery} placeholder="Filter the library" width={320} />
            </div>
          </div>
          <div style={section}>
            <div className="h6">Composer</div>
            <div style={row}>
              <KindSwitch kind={kind} onKind={setKind} />
              <ToolbarPill ref={pill} text="Qwen3 8B" icon="models" expanded={menu} onClick={() => setMenu(!menu)} />
              <ToolbarPill text="Locked" locked onClick={() => {}} />
              <ToolbarPill text="Web" emphasised chevron={false} onClick={() => {}} />
              <ToolbarPill text="Off" enabled={false} onClick={() => {}} />
              <PromptActionIconButton isGenerating={false} isEnabled onSend={() => say("Send")} onCancel={() => {}} />
              <PromptActionIconButton isGenerating={false} isEnabled={false} onSend={() => {}} onCancel={() => {}} />
              <PromptActionIconButton isGenerating isEnabled onSend={() => {}} onCancel={() => say("Stop")} />
              <PromptActionIconButton isGenerating={false} isLoading isEnabled onSend={() => {}} onCancel={() => {}} />
              <DropdownMenu anchor={pill} open={menu} onClose={() => setMenu(false)}>
                <StyledMenuItem text="Qwen3 8B" icon="check" fontWeight={500} onClick={() => setMenu(false)} />
                <StyledMenuItem text="More models…" icon="download" onClick={() => setMenu(false)} />
                <MenuDivider />
                <StyledMenuItem text="Delete" icon="trash" isDestructive onClick={() => setMenu(false)} />
              </DropdownMenu>
            </div>
          </div>
          <div style={section}>
            <div className="h6">Marks, progress</div>
            <div style={row}>
              <FanMark size={24} />
              <FanMark size={48} />
              {(["SUNRISE", "DAY", "SUNSET", "NIGHT"] as const).map((p) => (
                <TimeOfDayIcon key={p} part={p} size={32} />
              ))}
              <Spinner />
              <div style={{ width: 200 }}>
                <ProgressBar progress={0.4} height={8} />
              </div>
              <div style={{ width: 200 }}>
                <ProgressBar progress={null} />
              </div>
            </div>
          </div>
          <div style={section}>
            <Greeting subtitle="What should Nook build?" />
          </div>
          <div style={{ ...section, maxWidth: 640 }}>
            <SettingsSubNav options={["Library", "Browse", "Workers"]} selectedIndex={sub} onOptionSelected={setSub} />
            <SettingsGroup title="A group">
              <SettingsDropdown title="Web access" description="A dropdown row." options={["On", "Off"]} selectedValue="On" onOptionSelect={() => {}} />
              <SettingsRowDivider />
              <SettingsItemRow title="A toggle row" description="Toggle is new in the port.">
                <Toggle checked={on} onChange={setOn} />
              </SettingsItemRow>
              <SettingsRowDivider />
              <SettingsItemRow title="Actions">
                <div style={row}>
                  <SettingsAction text="Quiet" onClick={() => {}} />
                  <SettingsAction text="Primary" primary onClick={() => {}} />
                  <SettingsAction text="Danger" danger onClick={() => {}} />
                </div>
              </SettingsItemRow>
            </SettingsGroup>
          </div>
        </div>
      </PagePane>
    </div>
  );
}
