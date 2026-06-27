CREATE TABLE vpic.wmi (
    id integer NOT NULL,
    wmi character varying(6) NOT NULL,
    manufacturerid integer,
    makeid integer,
    vehicletypeid integer,
    createdon timestamp without time zone,
    updatedon timestamp without time zone,
    countryid integer,
    publicavailabilitydate timestamp without time zone,
    trucktypeid integer,
    processedon timestamp without time zone,
    noncompliant boolean DEFAULT false,
    noncompliantsetbyovsc boolean DEFAULT false
);
