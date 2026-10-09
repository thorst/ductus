import { Link } from "@tanstack/react-router";

export function NotFound() {
  return (
    <div className="mx-auto max-w-md py-16 text-center">
      <h1 className="text-xl font-semibold">Nothing here</h1>
      <p className="mt-2 text-muted">This page doesn't exist (maybe a link from another version).</p>
      <Link to="/" className="mt-4 inline-block">
        Go to the overview
      </Link>
    </div>
  );
}
