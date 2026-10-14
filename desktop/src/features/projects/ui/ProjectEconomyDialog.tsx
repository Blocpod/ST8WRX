import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { useCommunities } from "@/features/communities/useCommunities";
import type { Project } from "../projectModels";
import {
  latestEconomyEvents,
  type ProjectEconomy,
  type SubmitEconomyAction,
} from "../economy";
import { invokeTauri } from "@/shared/api/tauri";
import { useIdentityQuery } from "@/shared/api/hooks";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/shared/ui/tabs";
import { ProjectEconomyForms } from "./ProjectEconomyForms";
import { ComputeRecords, ContributionRecords } from "./ProjectEconomyRecords";

export function ProjectEconomyDialog({ project }: { project: Project }) {
  const [open, setOpen] = React.useState(false);
  const [pending, setPending] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [success, setSuccess] = React.useState<string | null>(null);
  const { activeCommunity } = useCommunities();
  const identity = useIdentityQuery();
  const query = useQuery({
    queryKey: [
      "st8-project-economy",
      activeCommunity?.id,
      activeCommunity?.relayUrl,
      identity.data?.pubkey,
      project.projectAddress,
    ],
    queryFn: async () => {
      if (!activeCommunity?.relayUrl || !identity.data?.pubkey) {
        throw new Error(
          "An active community and signing identity are required.",
        );
      }
      const data = await invokeTauri<ProjectEconomy>(
        "get_st8_project_economy",
        {
          project: project.projectAddress,
          expectedRelayUrl: activeCommunity.relayUrl,
          expectedSignerPubkey: identity.data.pubkey,
        },
      );
      return {
        ...data,
        policies: latestEconomyEvents(data.policies),
        contribution_ledger: latestEconomyEvents(data.contribution_ledger),
        compute_receipts: latestEconomyEvents(data.compute_receipts),
        compute_settlements: latestEconomyEvents(data.compute_settlements),
      };
    },
    enabled:
      open &&
      !project.legacy &&
      Boolean(activeCommunity?.relayUrl && identity.data?.pubkey),
    refetchInterval: open ? 30_000 : false,
    retry: false,
  });
  const reportError = (value: unknown) =>
    setError(value instanceof Error ? value.message : String(value));
  const submit: SubmitEconomyAction = async (action, compute = false) => {
    if (!query.data || pending) return;
    setPending(true);
    setError(null);
    setSuccess(null);
    try {
      const result = await invokeTauri<{
        accepted: boolean;
        event_id: string;
        message: string;
      }>(compute ? "st8_compute_action" : "st8_contribution_action", {
        project: project.projectAddress,
        expectedRelayUrl: query.data.relay_url,
        expectedSignerPubkey: query.data.viewer_pubkey,
        action,
      });
      if (!result.accepted)
        throw new Error(result.message || "The relay rejected this action.");
      setSuccess(
        "Signed event accepted. Ledger updates may take a moment; refresh to check progress.",
      );
      await query.refetch();
    } catch (cause) {
      reportError(
        `${cause instanceof Error ? cause.message : String(cause)} Refresh records before retrying; the relay may already have accepted the signed event.`,
      );
    } finally {
      setPending(false);
    }
  };
  if (project.legacy) return null;
  return (
    <>
      <Button
        data-testid="open-project-economy"
        onClick={() => setOpen(true)}
        size="sm"
        variant="outline"
      >
        Contributions & compute
      </Button>
      <Dialog
        open={open}
        onOpenChange={(value) => {
          if (!pending) setOpen(value);
        }}
      >
        <DialogContent
          className="max-h-[85vh] max-w-3xl overflow-y-auto"
          data-testid="project-economy-dialog"
        >
          <DialogHeader>
            <DialogTitle>{project.name} · Contributions & compute</DialogTitle>
            <DialogDescription>
              Submit work, approve Contribution Units, and inspect testnet
              accounting.
            </DialogDescription>
          </DialogHeader>
          <div className="flex items-center justify-between gap-3">
            <p className="text-xs text-muted-foreground">
              Private beta · testnet proofs
            </p>
            <Button
              disabled={query.isFetching || pending}
              onClick={() => void query.refetch()}
              size="sm"
              variant="ghost"
            >
              Refresh
            </Button>
          </div>
          {query.isPending && (
            <p role="status" className="text-sm">
              Loading signed project records…
            </p>
          )}
          {query.isError && (
            <p role="alert" className="text-sm text-destructive">
              Could not load project accounting: {String(query.error)}
            </p>
          )}
          {identity.isError && (
            <p role="alert" className="text-sm text-destructive">
              Could not load your signing identity. Reconnect before submitting
              an accounting action.
            </p>
          )}
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          {success && (
            <p role="status" className="text-sm">
              {success}
            </p>
          )}
          {query.data && (
            <>
              {query.data.has_more && (
                <p className="text-sm text-muted-foreground">
                  Showing the latest 100 records per kind. Older history remains
                  available through the CLI; this view does not show a complete
                  balance.
                </p>
              )}
              <Tabs defaultValue="contributions">
                <TabsList>
                  <TabsTrigger value="contributions">Contributions</TabsTrigger>
                  <TabsTrigger value="submit">Submit & govern</TabsTrigger>
                  <TabsTrigger value="compute">Compute</TabsTrigger>
                </TabsList>
                <TabsContent value="contributions">
                  <ContributionRecords
                    data={query.data}
                    pending={pending}
                    submit={submit}
                  />
                </TabsContent>
                <TabsContent value="submit">
                  <ProjectEconomyForms
                    data={query.data}
                    onError={reportError}
                    pending={pending}
                    submit={submit}
                  />
                </TabsContent>
                <TabsContent value="compute">
                  <ComputeRecords
                    data={query.data}
                    pending={pending}
                    submit={submit}
                  />
                </TabsContent>
              </Tabs>
            </>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
