import {test} from "node:test";
import assert from "node:assert/strict";
import {telemetryBatch} from "../src/telemetry.ts";
test("browser analytics cannot forward secrets or arbitrary context",()=>{
 const body=telemetryBatch({events:[{type:"error",data:{message:"ask_secret",url:"https://example/?token=secret",duration_ms:Infinity},metadata:{password:"secret"}}]});
 assert.deepEqual(body,{table:"remindtelemetry",events:[{source:"web",event:"error",step:"browser",success:false,duration_ms:0,status_code:null}]});
});
