/**
 * An icon from src/assets/icons by name ("chat", "code", "plus"...), tinted with the current text
 * colour like Compose's `tint`: the files draw in black, which is swapped for currentColor.
 */
const files = import.meta.glob("../assets/icons/*.svg", { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;

const icons: Record<string, string> = {};
for (const [path, raw] of Object.entries(files)) {
  const name = path.split("/").pop()!.replace(/\.svg$/, "");
  icons[name] = raw
    // The files' own <title> would show as a browser tooltip ("Controls Pause Streamline Icon...").
    .replace(/<title>[\s\S]*?<\/title>/gi, "")
    .replace(/(stroke|fill)="(black|#000|#000000)"/gi, '$1="currentColor"')
    .replace(/<svg([^>]*?)\swidth="[^"]*"/, "<svg$1")
    .replace(/<svg([^>]*?)\sheight="[^"]*"/, "<svg$1")
    .replace(/<svg/, '<svg width="100%" height="100%"');
}

export type IconName = string;

export function Icon({
  name,
  size = 16,
  color,
  className,
  title,
}: {
  name: IconName;
  size?: number;
  color?: string;
  className?: string;
  title?: string;
}) {
  const svg = icons[name];
  if (!svg && import.meta.env.DEV) console.warn(`No icon named ${name}`);
  return (
    <span
      className={className}
      title={title}
      aria-hidden={title ? undefined : true}
      style={{ display: "inline-flex", width: size, height: size, flex: "none", color }}
      dangerouslySetInnerHTML={{ __html: svg ?? "" }}
    />
  );
}

export const iconNames = Object.keys(icons);
