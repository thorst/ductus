-- 0.5.8.5: people who sign in with a directory (LDAP / Active Directory). Their accounts are
-- `users` rows with source 'ldap', made at their first sign-in; role, permissions, name and email
-- come from the directory at each sign-in and every `auth.ldap.refresh`.

-- When the directory was last asked about this person (sign-in or refresh).
ALTER TABLE users ADD COLUMN directory_checked_at timestamptz;
-- When the directory last said no (removed from every Ductus group, disabled or deleted there):
-- the account can't act, like a disabled one, until a directory check says yes again. Separate
-- from `disabled_at`, which is an admin's decision here.
ALTER TABLE users ADD COLUMN directory_refused_at timestamptz;
