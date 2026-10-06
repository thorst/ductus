// Bridge from @ductus/sdk to the engine's native operations.
const ops = Deno.core.ops;

globalThis.__ductus_ops = Object.freeze({
  dbQuery: (resource, sql, params) => ops.op_ductus_db_query(resource, sql, params),
  dbExecute: (resource, sql, params) => ops.op_ductus_db_execute(resource, sql, params),
  fetch: (request) => ops.op_ductus_fetch(request),
  variable: (name) => ops.op_ductus_variable(name),
  secret: (name) => ops.op_ductus_secret(name),
});
