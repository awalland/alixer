use anyhow::{bail,Result};
use alixer_core::{*,reconcile::Checkpoint};
use rusqlite::{Connection,params};
use serde::{Deserialize,Serialize};
use std::path::Path;
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct Saved {
    pub scene:Scene,
    pub checkpoint:Checkpoint,
    pub conflicts:Vec<Conflict>,
    #[serde(default)]
    pub retention: crate::retention::Retention,
}
pub struct Store {db:Connection}
impl Store {
    pub fn open(path:&Path)->Result<Self>{
        let db=Connection::open(path)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS metadata(version INTEGER NOT NULL); INSERT INTO metadata SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM metadata); CREATE TABLE IF NOT EXISTS state(id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL); CREATE TABLE IF NOT EXISTS operations(id INTEGER PRIMARY KEY, revision INTEGER NOT NULL, command TEXT NOT NULL, confirmed INTEGER NOT NULL DEFAULT 0);")?;
        let version:i64=db.query_row("SELECT version FROM metadata",[],|r|r.get(0))?;
        if version!=1{bail!("Unsupported database schema {version}; refusing to overwrite");}
        Ok(Self{db})
    }
    pub fn load(&self)->Result<Saved>{
        let mut stmt=self.db.prepare("SELECT json FROM state WHERE id=1")?;
        let mut rows=stmt.query([])?;
        if let Some(row)=rows.next()?{
            let json:String=row.get(0)?;
            let mut saved:Saved=serde_json::from_str(&json)?;
            saved.scene.validate()?;
            if !saved.retention.history_imported {
                // Existing explicit edits are durable too, including a route moved
                // to another device whose current shape still looks like a default.
                let mut history = self.db.prepare("SELECT command FROM operations ORDER BY id")?;
                let commands = history.query_map([], |r| r.get::<_, String>(0))?;
                for command in commands {
                    if let Ok(alixer_protocol::Command::Edit(edit)) = serde_json::from_str(&command?) {
                        saved.retention.protect_edit(&saved.scene, &edit);
                    }
                }
                saved.retention.history_imported = true;
            }
            Ok(saved)
        }else{Ok(Saved::default())}
    }
    pub fn save(&mut self,saved:&Saved,command:Option<&str>)->Result<()>{
        let tx=self.db.transaction()?;
        tx.execute("INSERT INTO state(id,json) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",[serde_json::to_string(saved)?])?;
        if let Some(command)=command{tx.execute("INSERT INTO operations(revision,command) VALUES(?1,?2)",params![i64::try_from(saved.scene.revision)?,command])?;}
        tx.commit()?;Ok(())
    }
    pub fn pending(&self)->Result<i64>{Ok(self.db.query_row("SELECT COUNT(*) FROM operations WHERE confirmed=0",[],|r|r.get(0))?)}
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn legacy_edit_history_protects_default_looking_custom_routes() {
        let mut db = Store::open(Path::new(":memory:")).unwrap();
        let mut saved = Saved::default();
        let strip = Strip::new("browser", StripKind::Stream, vec!["MONO".into()]);
        let id = strip.id;
        saved.scene.strips.push(strip);
        let command = alixer_protocol::Command::Edit(Edit::PutRoute { route: Route {
            id: Id::new_v4(), source: id, destination: Destination::Output("headphones".into()),
            source_channel: 0, target_channel: 0, gain: 1.0, tap: Tap::Post,
        }});
        db.save(&saved, Some(&serde_json::to_string(&command).unwrap())).unwrap();
        let mut legacy = serde_json::to_value(&saved).unwrap();
        legacy.as_object_mut().unwrap().remove("retention");
        db.db.execute("UPDATE state SET json=?1", [legacy.to_string()]).unwrap();
        let mut loaded = db.load().unwrap();
        assert!(loaded.retention.history_imported);
        assert!(loaded.retention.protected.contains(&id));
        loaded.retention.absent_since.insert(id, 1234);
        db.save(&loaded, None).unwrap();
        let reloaded = db.load().unwrap();
        assert_eq!(reloaded.retention.absent_since[&id], 1234);
        assert!(reloaded.retention.protected.contains(&id));
    }
    #[test]fn committed_intent_survives_without_an_audio_confirmation(){
        let root=std::env::temp_dir().join(format!("alixer-store-{}",Id::new_v4()));std::fs::create_dir_all(&root).unwrap();
        {let mut db=Store::open(&root.join("state.db")).unwrap();let mut saved=Saved::default();saved.scene.edit(Edit::AddBus{name:"Music".into(),aux:false,channels:vec!["FL".into(),"FR".into()]}).unwrap();db.save(&saved,Some("AddBus")).unwrap();}
        let db=Store::open(&root.join("state.db")).unwrap();assert_eq!(db.load().unwrap().scene.strips[0].name,"Music");assert_eq!(db.pending().unwrap(),1);drop(db);std::fs::remove_dir_all(root).unwrap();
    }
}
