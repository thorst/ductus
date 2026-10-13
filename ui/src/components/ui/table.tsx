import type { ComponentProps } from "react";
import { cn } from "@/lib/utils";

export function Table({ className, ...props }: ComponentProps<"table">) {
  // `relative`: an sr-only span inside a wide table is absolutely placed, and escapes the scroll box
  // (widening the page) unless this is its containing block.
  return (
    <div className="relative overflow-x-auto">
      <table className={cn("w-full border-collapse text-sm", className)} {...props} />
    </div>
  );
}

export function Th({ className, ...props }: ComponentProps<"th">) {
  return (
    <th
      className={cn("border-b px-3 py-2 text-left text-xs font-medium tracking-wide text-muted uppercase", className)}
      {...props}
    />
  );
}

export function Td({ className, ...props }: ComponentProps<"td">) {
  return <td className={cn("border-b px-3 py-2 align-top", className)} {...props} />;
}
