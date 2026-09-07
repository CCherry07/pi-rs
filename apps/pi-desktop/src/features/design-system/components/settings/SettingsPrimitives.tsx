import type { ComponentPropsWithoutRef, ReactNode } from "react";
import { joinClassNames } from "../classNames";

type SettingsSectionProps = {
  title: ReactNode;
  subtitle?: ReactNode;
  className?: string;
  children: ReactNode;
};

export function SettingsSection({
  title,
  subtitle,
  className,
  children,
}: SettingsSectionProps) {
  return (
    <section className={joinClassNames("settings-section", className)}>
      <div className="settings-section-title">{title}</div>
      {subtitle ? <div className="settings-section-subtitle">{subtitle}</div> : null}
      {children}
    </section>
  );
}

type SettingsSubsectionProps = {
  title: ReactNode;
  subtitle?: ReactNode;
  className?: string;
};

export function SettingsSubsection({ title, subtitle, className }: SettingsSubsectionProps) {
  return (
    <div className={className}>
      <div className="settings-subsection-title">{title}</div>
      {subtitle ? <div className="settings-subsection-subtitle">{subtitle}</div> : null}
    </div>
  );
}

type SettingsToggleRowProps = {
  title: ReactNode;
  subtitle?: ReactNode;
  className?: string;
  children: ReactNode;
};

export function SettingsToggleRow({
  title,
  subtitle,
  className,
  children,
}: SettingsToggleRowProps) {
  return (
    <div className={joinClassNames("settings-toggle-row", className)}>
      <div>
        <div className="settings-toggle-title">{title}</div>
        {subtitle ? <div className="settings-toggle-subtitle">{subtitle}</div> : null}
      </div>
      {children}
    </div>
  );
}

type SettingsToggleSwitchProps = Omit<
  ComponentPropsWithoutRef<"button">,
  "type" | "children" | "className" | "aria-pressed"
> & {
  pressed: boolean;
  className?: string;
};

export function SettingsToggleSwitch({
  pressed,
  className,
  ...props
}: SettingsToggleSwitchProps) {
  return (
    <button
      type="button"
      className={joinClassNames("settings-toggle", pressed && "on", className)}
      aria-pressed={pressed}
      {...props}
    >
      <span className="settings-toggle-knob" />
    </button>
  );
}
