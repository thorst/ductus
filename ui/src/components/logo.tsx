/** The Ductus mark: two lines crossing over, a message's route through the engine. */
export function Logo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" aria-hidden className={className}>
      <rect width="32" height="32" rx="7" fill="var(--accent)" />
      <path
        d="M7 10h7c5 0 5 12 10 12h1M7 22h7c5 0 5-12 10-12h1"
        fill="none"
        stroke="var(--accent-fg)"
        strokeWidth="2.6"
        strokeLinecap="round"
      />
    </svg>
  );
}
