import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** Class names, later ones winning over earlier Tailwind classes they conflict with. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
