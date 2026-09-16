// Reduce third-party analytics before any network handoff: never forward URLs,
// error messages, DOM attributes, identifiers, user input or arbitrary metadata.
export function telemetryBatch(body: any) {
  const names: Record<string,string> = {page_view:"page_view",page_exit:"navigation",navigation:"navigation",click:"interaction",scroll:"interaction",error:"error",network_error:"error",network:"request_completed",timing:"performance"};
  return {table:"remindtelemetry",events:(Array.isArray(body?.events)?body.events:[]).slice(0,40).map((e:any)=>{
    const kind=names[e.type] || "interaction";
    const raw=Number(e.data?.duration_ms ?? e.data?.elapsed_ms ?? e.data?.load_ms ?? 0);
    const status=Number(e.data?.status);
    return {source:"web",event:kind,step:"browser",success:kind!=="error" && !(status>=400),duration_ms:Number.isFinite(raw)?Math.min(86400000,Math.max(0,Math.round(raw))):0,status_code:Number.isInteger(status)&&status>=100&&status<=599?status:null};
  })};
}
