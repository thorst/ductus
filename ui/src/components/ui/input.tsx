import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

export function Input({ className, ...props }: ComponentProps<"input">) {
  return (
    <input
      className={cn(
        "h-9 w-full rounded-md border bg-panel px-3 text-sm placeholder:text-muted focus-visible:border-accent",
        className,
      )}
      {...props}
    />
  );
}
