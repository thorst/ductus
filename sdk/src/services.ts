// Engine services for interface code: databases, environment variables, secrets.
//
//   const rows = await db("patient_db").query("SELECT name FROM patients WHERE mrn = $1", [mrn]);
//   const url = env("EPIC_URL");

type Param = string | number | boolean | null;
type Row = Record<string, unknown>;

interface Ops {
  dbQuery(resource: string, sql: string, params: Param[]): Promise<Row[]>;
  dbExecute(resource: string, sql: string, params: Param[]): Promise<number>;
  variable(name: string): string | null;
  secret(name: string): string | null;
}

function ops(): Ops {
  const o = (globalThis as { __ductus_ops?: Ops }).__ductus_ops;
  if (!o) throw new Error("engine services are not available");
  return o;
}

/** A database defined in resources.yaml. */
export interface Database {
  /**
   * Rows as objects. Use $1, $2... placeholders on Postgres and SQL Server alike; cast in SQL if
   * needed: `$1::int` (Postgres), `CAST($1 AS varchar(20))` (SQL Server sends text as nvarchar, so
   * cast it when comparing with an indexed varchar column).
   */
  query<T = Row>(sql: string, params?: Param[]): Promise<T[]>;
  /** The first row, or null. */
  one<T = Row>(sql: string, params?: Param[]): Promise<T | null>;
  /** Run a statement; returns rows affected. */
  execute(sql: string, params?: Param[]): Promise<number>;
}

/** A database defined in resources.yaml, by name. */
export function db(name: string): Database {
  return {
    query: <T>(sql: string, params: Param[] = []) => ops().dbQuery(name, sql, params) as Promise<T[]>,
    one: async <T>(sql: string, params: Param[] = []) =>
      ((await ops().dbQuery(name, sql, params))[0] as T | undefined) ?? null,
    execute: (sql: string, params: Param[] = []) => ops().dbExecute(name, sql, params),
  };
}

/** A variable from environments/<env>.yaml. Throws if it isn't defined. */
export function env(name: string): string {
  const value = ops().variable(name);
  if (value === null || value === undefined) throw new Error(`environment variable "${name}" is not defined`);
  return value;
}

/** A secret from the environment's secret store. Throws if it isn't defined. */
export function secret(name: string): string {
  const value = ops().secret(name);
  if (value === null || value === undefined) throw new Error(`secret "${name}" is not defined`);
  return value;
}
