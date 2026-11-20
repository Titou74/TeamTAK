import { useQueryClient } from "@tanstack/react-query";
import { MailCheckIcon, MailIcon, ServerIcon } from "lucide-react";
import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Brand } from "@/components/app-shell";
import { LanguageSwitcher } from "@/components/language-switcher";
import { SiteFooter } from "@/components/site-footer";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { adoptAccountLanguage, currentLanguage } from "@/i18n";
import { api, ApiError } from "@/lib/api";
import { useErrorText, usePublicConfig } from "@/lib/hooks";
import { useLoginProofs } from "@/lib/login-proof";

/** The address as the server compares it. */
const normalized = (email: string) => email.trim().toLowerCase();

/**
 * Passwordless sign-in: an e-mail address, then the one-time code sent to it. An invisible
 * anti-bot challenge is solved in the background while the address is typed, and another
 * code for the same address can only be asked for once a countdown has run out.
 */
export function LoginPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const errorText = useErrorText();
  const config = usePublicConfig({ refetchOnFocus: true });
  const serverName = config.data?.server_name;
  // Unknown until the configuration is in: the page then says nothing about sign-ups.
  const invitationOnly = config.data?.invitation_only === true;
  const proofs = useLoginProofs();
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [step, setStep] = useState<"email" | "code">("email");
  // False when the user says they already hold a code (rescue command): nothing was sent.
  const [codeSent, setCodeSent] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Until when no new code can be sent to this address.
  const [cooldown, setCooldown] = useState<{ email: string; until: number } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const waitSeconds =
    cooldown && cooldown.email === normalized(email) ? Math.max(0, Math.ceil((cooldown.until - now) / 1000)) : 0;

  useEffect(() => {
    if (!cooldown) return;
    const timer = window.setInterval(() => {
      const current = Date.now();
      setNow(current);
      if (current >= cooldown.until) setCooldown(null);
    }, 250);
    return () => window.clearInterval(timer);
  }, [cooldown]);

  function startCooldown(seconds: number) {
    const current = Date.now();
    setNow(current);
    setCooldown({ email: normalized(email), until: current + seconds * 1000 });
  }

  function showCodeStep() {
    setCodeSent(true);
    setStep("code");
    setCode("");
  }

  async function sendCode(event?: FormEvent) {
    event?.preventDefault();
    setError(null);
    // The code sent moments ago to this address is still good: no need for another one.
    if (step === "email" && waitSeconds > 0) {
      showCodeStep();
      return;
    }
    setPending(true);
    try {
      const proof = await proofs.take();
      await api.requestCode(email, currentLanguage(), proof);
      startCooldown(config.data?.resend_cooldown_seconds ?? 60);
      if (step === "code") toast.success(t("login.codeSent"));
      showCodeStep();
    } catch (e) {
      if (e instanceof ApiError && e.code === "resend_too_soon") {
        startCooldown(e.retryAfter ?? config.data?.resend_cooldown_seconds ?? 60);
        // Asked from another tab, or before the page was reloaded: that code is the one to type.
        if (step === "email") {
          toast.info(t("login.codeAlreadySent"));
          showCodeStep();
          return;
        }
      }
      setError(errorText(e));
    } finally {
      setPending(false);
    }
  }

  async function verify(event: FormEvent) {
    event.preventDefault();
    setPending(true);
    setError(null);
    try {
      const me = await api.verifyCode(email, code, currentLanguage());
      adoptAccountLanguage(me.account.language);
      queryClient.setQueryData(["me"], me);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="flex min-h-svh flex-col">
      <header className="mx-auto flex h-14 w-full max-w-5xl items-center justify-between px-4">
        <Brand />
        <LanguageSwitcher />
      </header>
      <main className="flex flex-1 flex-col items-center justify-center gap-4 px-4 pb-8">
        {/* Which server this is, when an organisation runs several. */}
        {serverName && (
          <p className="flex max-w-sm items-center justify-center gap-2 text-center text-lg font-semibold tracking-tight">
            <ServerIcon className="size-5 shrink-0 text-muted-foreground" aria-hidden />
            <span className="min-w-0 break-words">{serverName}</span>
          </p>
        )}
        <Card className="w-full max-w-sm">
          <CardHeader>
            <CardTitle className="text-xl">{step === "email" ? t("login.title") : t("login.codeTitle")}</CardTitle>
            <CardDescription>
              {step === "email"
                ? t("login.intro")
                : codeSent
                  ? invitationOnly
                    ? t("login.codeIntroInvitationOnly", { email, minutes: config.data?.code_ttl_minutes ?? 10 })
                    : t("login.codeIntro", { email, minutes: config.data?.code_ttl_minutes ?? 10 })
                  : t("login.haveCodeIntro", { email })}
            </CardDescription>
          </CardHeader>
          <CardContent>
            {step === "email" ? (
              <form key="email" onSubmit={(e) => void sendCode(e)} className="space-y-4">
                {invitationOnly && (
                  <Alert>
                    <MailCheckIcon />
                    <AlertDescription>{t("login.invitationOnly")}</AlertDescription>
                  </Alert>
                )}
                <div className="space-y-2">
                  <Label htmlFor="email">{t("login.email")}</Label>
                  <Input
                    id="email"
                    type="email"
                    autoComplete="email"
                    autoFocus
                    required
                    value={email}
                    onChange={(e) => setEmail(e.target.value)}
                    placeholder={t("login.emailPlaceholder")}
                  />
                </div>
                {error && (
                  <Alert variant="destructive">
                    <AlertDescription>{error}</AlertDescription>
                  </Alert>
                )}
                <Button type="submit" className="w-full" size="lg" disabled={pending || !email}>
                  <MailIcon />
                  {pending ? t("common.working") : t("login.send")}
                </Button>
                {/* For a code obtained another way, the administrator's rescue command: offered
                    only while such a code waits to be used, so not once the first administrator
                    is in. */}
                {config.data?.rescue_code_pending && (
                  <div className="text-center text-sm">
                    <Button
                      type="button"
                      variant="link"
                      className="px-0"
                      disabled={!email}
                      onClick={() => {
                        setCodeSent(false);
                        setStep("code");
                        setError(null);
                      }}
                    >
                      {t("login.haveCode")}
                    </Button>
                  </div>
                )}
              </form>
            ) : (
              <form key="code" onSubmit={(e) => void verify(e)} className="space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="code">{t("login.code")}</Label>
                  <Input
                    id="code"
                    inputMode="numeric"
                    autoComplete="one-time-code"
                    autoFocus
                    required
                    maxLength={12}
                    value={code}
                    onChange={(e) => setCode(e.target.value)}
                    className="h-12 text-center font-mono text-2xl tracking-[0.4em]"
                  />
                </div>
                {error && (
                  <Alert variant="destructive">
                    <AlertDescription>{error}</AlertDescription>
                  </Alert>
                )}
                <Button type="submit" className="w-full" size="lg" disabled={pending || code.trim().length < 6}>
                  {pending ? t("common.working") : t("login.verify")}
                </Button>
                <div className="flex justify-between text-sm">
                  <Button
                    type="button"
                    variant="link"
                    className="px-0 tabular-nums"
                    disabled={pending || waitSeconds > 0}
                    onClick={() => void sendCode()}
                  >
                    {waitSeconds > 0 ? t("login.resendIn", { seconds: waitSeconds }) : t("login.resend")}
                  </Button>
                  <Button
                    type="button"
                    variant="link"
                    className="px-0"
                    onClick={() => {
                      setStep("email");
                      setError(null);
                    }}
                  >
                    {t("login.changeEmail")}
                  </Button>
                </div>
              </form>
            )}
          </CardContent>
        </Card>
      </main>
      <SiteFooter />
    </div>
  );
}
