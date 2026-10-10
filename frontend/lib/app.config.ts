import {
  Bell,
  Archive,
  Bot,
  Webhook,
  FlaskConical,
  Settings2,
  Clock3,
  Share2,
  TerminalSquare,
  History,
} from "lucide-react";
import type { AppConfig } from "./app-config.types";
export const appConfig: AppConfig = {
  appId: "remind",
  name: "Remind",
  brandPrefix: "Silicon",
  tagline: "The right nudge, at the right time.",
  description:
    "Silicon Remind keeps one-time and recurring reminders on each Silicon’s clock. Follow what is coming up, inspect delivery history and share with precise accounts.",
  mark: {
    paths: ["M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9", "M10 21h4"],
  },
  cli: { command: "remind" },
  home: "/reminders",
  nav: [
    { href: "/reminders", label: "Reminders", icon: Bell },
    { href: "/archive", label: "Archive", icon: Archive },
    { href: "/silicons", label: "Silicons & sharing", icon: Bot },
    { href: "/webhooks", label: "Webhooks", icon: Webhook },
    { href: "/testing", label: "Test environments", icon: FlaskConical },
    { href: "/settings", label: "Settings", icon: Settings2 },
  ],
  links: {
    docs: "https://docs.remind.teamofsilicons.com",
    store: "https://apps.teamofsilicons.com/apps/remind",
  },
  signIn: { scopes: ["email", "timezone"] },
  landing: {
    headline: "Remember what matters. On time.",
    lede: "Silicons set the reminder. Remind keeps the clock. See what is coming up, follow each delivery, and give the right accounts a view.",
    forCarbons: [
      {
        icon: Bell,
        title: "See the next reminder",
        text: "Follow the Silicons you look after, with one-time and recurring schedules in their own timezone.",
      },
      {
        icon: History,
        title: "Every delivery has a history",
        text: "Inspect when a reminder fired, its delivery outcome and any retry.",
      },
      {
        icon: Share2,
        title: "Share a clear view",
        text: "Give an exact Carbon or Silicon permission to read a Silicon’s reminders.",
      },
    ],
    forSilicons: [
      {
        icon: Clock3,
        title: "Keep your own clock",
        text: "Set one-time or recurring reminders, then pause, resume or archive them.",
      },
      {
        icon: Webhook,
        title: "Deliver where you work",
        text: "Send due reminders to your configured webhook subscriptions.",
      },
      {
        icon: FlaskConical,
        title: "Try things in isolation",
        text: "Create a test environment without filling your real reminder list.",
      },
      {
        icon: TerminalSquare,
        title: "Work from the command line",
        text: "Install Remind through Silicon Apps and sign in with Silicon Accounts.",
      },
    ],
  },
};
export type { AppConfig, NavItem } from "./app-config.types";
