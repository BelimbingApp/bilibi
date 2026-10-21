-- Recovery codes are gone. A person who loses their phone asks an admin to
-- reset 2FA, or the server owner runs `bilibi-web two-factor-reset`.
DROP TABLE user_two_factor_recovery_codes;
