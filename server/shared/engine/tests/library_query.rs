#![cfg(feature="runtime")]
use viptv_engine::{library::*,profile::ProfileConfiguration,store::Store,simkl,Error};
use serde_json::json;
#[test]
fn library_filters_page_real_data_and_preserve_unknown_personal_ratings_dates_and_counts(){
 let scope=ProfileConfiguration::local("local".into(),"one".into(),"One".into()).scope;
 let entry=|id,name:&str,genre:&str,added,watched|{let mut card=simkl::card(&json!({"title":name,"ids":{"simkl":id},"genres":[genre],"ratings":{"imdb":{"rating":9.7,"votes":100}}}),"movie").unwrap();card.added_at_millis=added;let mut statistics=LibraryStatistics::empty(card.identity.clone());statistics.watched=watched;statistics.user_rating=None;Entry{card,statistics}};
 let sources=vec![LibrarySource{id:"favorites".into(),label:"Saved titles".into(),availability:viptv_engine::capabilities::Availability::Available,reason:None}];let mut query=LibraryQuery::default();query.sort="name_desc".into();query.show="not_watched".into();query.genre=Some("Drama".into());
 let result=project(scope.clone(),0,query.clone(),vec![entry(1,"Alpha","Drama",Some(50),true),entry(2,"Zulu","Drama",None,false),entry(3,"Beta","Comedy",Some(100),false)],sources.clone(),200,None).unwrap();assert_eq!(result.cards.len(),1);assert_eq!(result.cards[0].title,"Zulu");assert!(result.statistics[0].user_rating.is_none());assert!(result.controls.iter().all(|c|c.id!="rated"&&c.id!="status"));
 query.rated=Some("rated_8_plus".into());assert!(matches!(project(scope,0,query,vec![],sources,200,None),Err(Error::InvalidQuery)));
}
#[test]
fn observed_native_completions_increment_once_per_rewatch_and_stay_profile_scoped(){
 let mut store=Store::memory().unwrap();let a=ProfileConfiguration::local("local".into(),"a".into(),"A".into());let b=ProfileConfiguration::local("local".into(),"b".into(),"B".into());store.save_profile(&a,None).unwrap();store.save_profile(&b,None).unwrap();let card=simkl::card(&json!({"title":"Title","ids":{"simkl":1}}),"movie").unwrap();store.save_favorite(&a.scope,&card).unwrap();store.save_favorite(&b.scope,&card).unwrap();
 for (position,now) in [(10000,1),(95000,2),(99000,3),(10000,4),(96000,5)]{store.record_playback(&a.scope,&card,position,100000,now).unwrap();}
 let result=store.library_entries(&a.scope).unwrap();assert_eq!(result[0].statistics.times_watched,Some(2));assert!(result[0].statistics.watched);assert_eq!(result[0].statistics.last_watched_millis,Some(5));let other=store.library_entries(&b.scope).unwrap();assert_eq!(other[0].statistics.times_watched,Some(0));assert!(!other[0].statistics.watched);
}
#[test]
fn empty_optional_filter_choices_mean_any_without_accepting_unknown_filters(){
 let query:LibraryQuery=serde_json::from_value(json!({"status":"","genre":"","rated":"","added":""})).unwrap();query.validate().unwrap();assert!(query.status.is_none()&&query.genre.is_none()&&query.rated.is_none()&&query.added.is_none());
 assert!(serde_json::from_value::<LibraryQuery>(json!({"invented_filter":true})).is_err());
 assert!(serde_json::from_value::<LibraryQuery>(json!({"status":"invented"})).unwrap().validate().is_err());
}
#[test]
fn provider_next_episode_marker_wins_over_counts_for_caught_up_filter(){
 let scope=ProfileConfiguration::local("local".into(),"one".into(),"One".into()).scope;let card=simkl::card(&json!({"title":"Skipped old episodes","ids":{"simkl":1}}),"tv").unwrap();let mut statistics=LibraryStatistics::empty(card.identity.clone());statistics.watched_episode_count=Some(3);statistics.aired_episode_count=Some(7);statistics.has_unwatched_aired_episodes=Some(false);let entry=Entry{card,statistics};let mut query=LibraryQuery::default();query.source="simkl".into();query.show="caught_up".into();assert_eq!(project(scope.clone(),0,query.clone(),vec![entry.clone()],vec![],100,None).unwrap().cards.len(),1);query.show="has_new_episode".into();assert!(project(scope,0,query,vec![entry],vec![],100,None).unwrap().cards.is_empty());
}
#[test]
fn episode_completions_do_not_invent_whole_series_rewatch_counts(){
 let mut store=Store::memory().unwrap();let config=ProfileConfiguration::local("local".into(),"p".into(),"P".into());store.save_profile(&config,None).unwrap();let parent=simkl::card(&json!({"title":"Series","ids":{"simkl":20}}),"tv").unwrap();store.save_favorite(&config.scope,&parent).unwrap();let mut episode=parent.clone();episode.parent_identity=Some(parent.identity.clone());episode.identity.id="verified-episode".into();episode.identity.episode=Some(viptv_engine::identity::EpisodeCoordinates{order:"aired".into(),season:Some(1),episode:1});store.record_playback(&config.scope,&episode,0,100000,1).unwrap();assert!(store.library_entries(&config.scope).unwrap()[0].statistics.last_watched_millis.is_none());store.record_playback(&config.scope,&episode,96000,100000,2).unwrap();let stats=&store.library_entries(&config.scope).unwrap()[0].statistics;assert_eq!(stats.watched_episode_count,Some(1));assert!(stats.times_watched.is_none());
}
