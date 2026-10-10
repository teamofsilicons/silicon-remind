import { Reminders } from "@/components/remind/reminders";
export default async function Page({searchParams}:{searchParams:Promise<{silicon?:string}>}) {const query=await searchParams;return <Reminders initialSilicon={query.silicon}/>;}
